// This tool replays delegated attestations over the p2p gossip network.
//
// It fetches delegate observation signatures from the wormholescan API for a
// given VAA ID, fetches the full message data from an EVM RPC node (using the
// chain config for contract addresses and default RPCs), recovers the original
// sent_timestamp by brute-forcing against the API signature, and broadcasts the
// original SignedDelegateObservation messages to the `delegated_attestation`
// gossip topic.
//
// Usage:
//
//	go run main.go \
//	  -vaaID 59/0000000000000000000000005d4c6f2235d508b1e5a324c078b66cda2f5e7d5d/5955 \
//	  -network mainnet
package main

import (
	"context"
	"encoding/base64"
	"encoding/json"
	"flag"
	"fmt"
	"io"
	"net/http"
	"os"
	"strconv"
	"strings"
	"time"

	"github.com/certusone/wormhole/node/pkg/common"
	"github.com/certusone/wormhole/node/pkg/p2p"
	gossipv1 "github.com/certusone/wormhole/node/pkg/proto/gossip/v1"
	"github.com/certusone/wormhole/node/pkg/watchers/evm"
	"github.com/certusone/wormhole/node/pkg/watchers/evm/connectors"
	ethCommon "github.com/ethereum/go-ethereum/common"
	ethcrypto "github.com/ethereum/go-ethereum/crypto"
	pubsub "github.com/libp2p/go-libp2p-pubsub"
	p2pcrypto "github.com/libp2p/go-libp2p/core/crypto"
	"github.com/wormhole-foundation/wormhole/sdk/vaa"
	"go.uber.org/zap"
	"google.golang.org/protobuf/proto"
)

var signedDelegateObservationPrefix = []byte("signed_delegate_observation_000000|")

// Use a dedicated FlagSet to avoid glog's vmodule flag panic on flag.Parse().
var fs = flag.NewFlagSet(os.Args[0], flag.ExitOnError)

var (
	vaaIDFlag      = fs.String("vaaID", "", "VAA ID in chain/emitter/sequence format (e.g. 59/0000000000000000000000005d4c6f2235d508b1e5a324c078b66cda2f5e7d5d/5955)")
	ethRPC         = fs.String("ethRPC", "", "EVM RPC endpoint URL (defaults to the PublicRPC from chain config)")
	ethContract    = fs.String("ethContract", "", "Wormhole core bridge contract address (defaults to chain config)")
	nodeKey        = fs.String("nodeKey", "/tmp/broadcast_delegate.key", "Path to p2p node key file")
	network        = fs.String("network", "mainnet", "Network: mainnet, testnet, devnet")
	bootstrapPeers = fs.String("bootstrapPeers", "", "Override bootstrap peers (comma-separated multiaddrs)")
	apiURL         = fs.String("apiURL", "https://api.wormholescan.io", "Wormholescan API base URL")
	p2pPort        = fs.Uint("port", 8998, "P2P listen port")
)

// delegateObservationAPIResponse represents a single entry from the wormholescan delegate observations API.
type delegateObservationAPIResponse struct {
	Sequence              uint64 `json:"sequence"`
	ID                    string `json:"id"`
	EmitterChain          uint32 `json:"emitterChain"`
	EmitterAddr           string `json:"emitterAddr"`
	Hash                  string `json:"hash"`
	TxHash                string `json:"txHash"`
	Payload               string `json:"payload"`
	DelegatedGuardianAddr string `json:"delegatedGuardianAddr"`
	Signature             string `json:"signature"`
	UpdatedAt             string `json:"updatedAt"`
	IndexedAt             string `json:"indexedAt"`
}

func main() {
	if err := fs.Parse(os.Args[1:]); err != nil {
		os.Exit(1)
	}

	if *vaaIDFlag == "" {
		fmt.Println("Error: -vaaID is required")
		fs.Usage()
		return
	}

	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()

	logger, _ := zap.NewDevelopment()

	// Parse VAA ID.
	chainID, emitter, sequence, err := parseVAAID(*vaaIDFlag)
	if err != nil {
		logger.Fatal("failed to parse VAA ID", zap.Error(err))
	}
	logger.Info("parsed VAA ID", zap.Uint16("chain", chainID), zap.String("emitter", emitter), zap.Uint64("sequence", sequence))

	// Determine network parameters.
	env, err := common.ParseEnvironment(*network)
	if err != nil {
		logger.Fatal("invalid network", zap.Error(err))
	}

	// Resolve contract address from chain config if not overridden.
	wormholeChainID := vaa.ChainID(chainID)
	var contractAddr ethCommon.Address
	if *ethContract != "" {
		contractAddr = ethCommon.HexToAddress(*ethContract)
	} else {
		contractAddr, err = evm.GetContractAddr(env, wormholeChainID)
		if err != nil {
			logger.Fatal("failed to get contract address from chain config (use -ethContract to override)",
				zap.Uint16("chainID", chainID), zap.Error(err))
		}
	}
	logger.Info("using contract address", zap.String("contract", contractAddr.Hex()))

	// Resolve RPC URL from chain config if not overridden.
	rpcURL := *ethRPC
	if rpcURL == "" {
		chainConfig, err := evm.GetChainConfigMap(env)
		if err != nil {
			logger.Fatal("failed to get chain config", zap.Error(err))
		}
		entry, ok := chainConfig[wormholeChainID]
		if !ok || entry.PublicRPC == "" {
			logger.Fatal("no public RPC in chain config (use -ethRPC to specify)",
				zap.Uint16("chainID", chainID))
		}
		rpcURL = entry.PublicRPC
	}
	logger.Info("using EVM RPC", zap.String("rpc", rpcURL))

	// Fetch delegate observations from wormholescan.
	observations, err := fetchDelegateObservations(*apiURL, chainID, emitter, sequence)
	if err != nil {
		logger.Fatal("failed to fetch delegate observations", zap.Error(err))
	}
	logger.Info("fetched delegate observations", zap.Int("count", len(observations)))

	if len(observations) == 0 {
		logger.Fatal("no delegate observations found for this VAA ID")
	}

	// Fetch the full message data from the EVM chain.
	txHashBytes, err := base64.StdEncoding.DecodeString(observations[0].TxHash)
	if err != nil {
		logger.Fatal("failed to decode txHash from API", zap.Error(err))
	}
	txHash := ethCommon.BytesToHash(txHashBytes)

	msgPub, err := fetchMessageFromEVM(ctx, logger, rpcURL, contractAddr, wormholeChainID, txHash, sequence)
	if err != nil {
		logger.Fatal("failed to fetch message from EVM", zap.Error(err))
	}
	logger.Info("fetched message from EVM",
		zap.Uint32("nonce", msgPub.Nonce),
		zap.Time("timestamp", msgPub.Timestamp),
		zap.Uint8("consistencyLevel", msgPub.ConsistencyLevel),
		zap.Uint64("sequence", msgPub.Sequence),
	)

	// Build gossip messages by recovering the original sent_timestamp for each observation.
	var messages [][]byte
	for i, obs := range observations {
		msg, err := buildGossipMessage(logger, obs, msgPub)
		if err != nil {
			logger.Error("failed to build gossip message, skipping",
				zap.Int("index", i),
				zap.String("delegatedGuardian", obs.DelegatedGuardianAddr),
				zap.Error(err),
			)
			continue
		}
		messages = append(messages, msg)
		logger.Info("built gossip message",
			zap.Int("index", i),
			zap.String("delegatedGuardian", obs.DelegatedGuardianAddr),
		)
	}

	if len(messages) == 0 {
		logger.Fatal("no valid gossip messages to broadcast")
	}

	// Set up p2p.
	networkID := p2p.GetNetworkId(env)
	peers := *bootstrapPeers
	if peers == "" {
		peers, err = p2p.GetBootstrapPeers(env)
		if err != nil {
			logger.Fatal("failed to get bootstrap peers (use -bootstrapPeers to override)", zap.Error(err))
		}
	}

	var priv p2pcrypto.PrivKey
	priv, err = common.GetOrCreateNodeKey(logger, *nodeKey)
	if err != nil {
		logger.Fatal("failed to load node key", zap.Error(err))
	}

	components := p2p.DefaultComponents()
	components.Port = *p2pPort

	h, err := p2p.NewHost(logger, ctx, networkID, peers, components, priv)
	if err != nil {
		logger.Fatal("failed to create p2p host", zap.Error(err))
	}
	defer h.Close()

	ps, err := pubsub.NewGossipSub(ctx, h)
	if err != nil {
		logger.Fatal("failed to create gossipsub", zap.Error(err))
	}

	topic := fmt.Sprintf("%s/%s", networkID, "delegated_attestation")
	logger.Info("joining topic", zap.String("topic", topic))

	th, err := ps.Join(topic)
	if err != nil {
		logger.Fatal("failed to join delegated_attestation topic", zap.Error(err))
	}
	defer th.Close()

	// Wait for peers.
	logger.Info("waiting for peers...", zap.String("peer_id", h.ID().String()))
	timeout := time.After(60 * time.Second)
	for len(th.ListPeers()) < 1 {
		select {
		case <-timeout:
			logger.Fatal("timed out waiting for peers")
		default:
			time.Sleep(100 * time.Millisecond)
		}
	}
	logger.Info("connected to peers", zap.Int("count", len(th.ListPeers())))

	// Broadcast each message.
	for i, msg := range messages {
		if err := th.Publish(ctx, msg); err != nil {
			logger.Error("failed to publish message", zap.Int("index", i), zap.Error(err))
		} else {
			logger.Info("published delegate observation",
				zap.Int("index", i),
				zap.Int("total", len(messages)),
			)
		}
	}

	logger.Info("done broadcasting", zap.Int("published", len(messages)))
}

// parseVAAID splits a "chain/emitter/sequence" string.
func parseVAAID(id string) (uint16, string, uint64, error) {
	parts := strings.Split(id, "/")
	if len(parts) != 3 {
		return 0, "", 0, fmt.Errorf("expected chain/emitter/sequence, got %q", id)
	}

	chainNum, err := strconv.ParseUint(parts[0], 10, 16)
	if err != nil {
		return 0, "", 0, fmt.Errorf("invalid chain ID %q: %w", parts[0], err)
	}

	emitter := parts[1]
	if len(emitter) != 64 {
		return 0, "", 0, fmt.Errorf("emitter address must be 64 hex chars, got %d", len(emitter))
	}

	seq, err := strconv.ParseUint(parts[2], 10, 64)
	if err != nil {
		return 0, "", 0, fmt.Errorf("invalid sequence %q: %w", parts[2], err)
	}

	return uint16(chainNum), emitter, seq, nil
}

// fetchDelegateObservations calls the wormholescan API to get delegate observations for a VAA ID.
func fetchDelegateObservations(baseURL string, chain uint16, emitter string, sequence uint64) ([]delegateObservationAPIResponse, error) {
	url := fmt.Sprintf("%s/api/v1/observations/delegate/%d/%s/%d", baseURL, chain, emitter, sequence)

	resp, err := http.Get(url) //nolint:gosec // URL is constructed from user input in a CLI tool.
	if err != nil {
		return nil, fmt.Errorf("HTTP request failed: %w", err)
	}
	defer resp.Body.Close()

	if resp.StatusCode != http.StatusOK {
		body, _ := io.ReadAll(resp.Body)
		return nil, fmt.Errorf("API returned status %d: %s", resp.StatusCode, string(body))
	}

	body, err := io.ReadAll(resp.Body)
	if err != nil {
		return nil, fmt.Errorf("failed to read response body: %w", err)
	}

	var observations []delegateObservationAPIResponse
	if err := json.Unmarshal(body, &observations); err != nil {
		return nil, fmt.Errorf("failed to unmarshal response: %w", err)
	}

	return observations, nil
}

// fetchMessageFromEVM connects to the EVM RPC, fetches the transaction receipt, and extracts
// the MessagePublication matching the given sequence number.
func fetchMessageFromEVM(
	ctx context.Context,
	logger *zap.Logger,
	rpcURL string,
	contract ethCommon.Address,
	chainID vaa.ChainID,
	txHash ethCommon.Hash,
	sequence uint64,
) (*common.MessagePublication, error) {
	ethConn, err := connectors.NewEthereumBaseConnector(ctx, "evm", rpcURL, contract, nil, logger)
	if err != nil {
		return nil, fmt.Errorf("failed to create EVM connector: %w", err)
	}

	_, _, msgs, err := evm.MessageEventsForTransaction(ctx, ethConn, contract, chainID, txHash)
	if err != nil {
		return nil, fmt.Errorf("failed to get message events for tx %s: %w", txHash.Hex(), err)
	}

	for _, msg := range msgs {
		if msg.Sequence == sequence {
			return msg, nil
		}
	}

	return nil, fmt.Errorf("no message with sequence %d found in tx %s (found %d messages)", sequence, txHash.Hex(), len(msgs))
}

// buildGossipMessage reconstructs the original SignedDelegateObservation from the API data
// and the full message fetched from EVM. It brute-forces the sent_timestamp by trying every
// second from indexedAt back 60 seconds to find the original serialized bytes that match
// the API signature.
func buildGossipMessage(
	logger *zap.Logger,
	obs delegateObservationAPIResponse,
	msgPub *common.MessagePublication,
) ([]byte, error) {
	sig, err := base64.StdEncoding.DecodeString(obs.Signature)
	if err != nil {
		return nil, fmt.Errorf("failed to decode signature: %w", err)
	}

	guardianAddrHex := strings.TrimPrefix(obs.DelegatedGuardianAddr, "0x")
	guardianAddr := ethCommon.HexToAddress(guardianAddrHex)

	updatedAt, err := time.Parse(time.RFC3339Nano, obs.UpdatedAt)
	if err != nil {
		return nil, fmt.Errorf("failed to parse updatedAt %q: %w", obs.UpdatedAt, err)
	}

	// Try to recover the original serialized bytes by brute-forcing sent_timestamp and
	// the unknown fields (verificationState, unreliable, isReobservation) that aren't
	// returned by the wormholescan API.
	var matchedBytes []byte
	startUnix := updatedAt.Unix()

	for delta := int64(-60); delta <= 300 && matchedBytes == nil; delta++ {
		candidateTimestamp := startUnix - delta

		for verificationState := uint32(0); verificationState < common.NumVariantsVerificationState && matchedBytes == nil; verificationState++ {
			for _, unreliable := range []bool{false, true} {
				for _, isReobs := range []bool{false, true} {
					delegateObs := &gossipv1.DelegateObservation{
						Timestamp:         uint32(msgPub.Timestamp.Unix()),
						Nonce:             msgPub.Nonce,
						EmitterChain:      uint32(msgPub.EmitterChain),
						EmitterAddress:    msgPub.EmitterAddress[:],
						Sequence:          msgPub.Sequence,
						ConsistencyLevel:  uint32(msgPub.ConsistencyLevel),
						Payload:           msgPub.Payload,
						TxHash:            msgPub.TxID,
						Unreliable:        unreliable,
						IsReobservation:   isReobs,
						VerificationState: verificationState,
						GuardianAddr:      guardianAddr.Bytes(),
						SentTimestamp:     candidateTimestamp,
					}

					candidateBytes, err := proto.Marshal(delegateObs)
					if err != nil {
						continue
					}

					digest := ethcrypto.Keccak256Hash(append(signedDelegateObservationPrefix, candidateBytes...))
					recoveredPubKey, err := ethcrypto.Ecrecover(digest.Bytes(), sig)
					if err != nil {
						continue
					}

					recoveredAddr := ethCommon.BytesToAddress(ethcrypto.Keccak256(recoveredPubKey[1:])[12:])
					if recoveredAddr == guardianAddr {
						matchedBytes = candidateBytes
						logger.Info("recovered delegate observation parameters",
							zap.Int64("sentTimestamp", candidateTimestamp),
							zap.Int64("delta", delta),
							zap.Uint32("verificationState", verificationState),
							zap.Bool("unreliable", unreliable),
							zap.Bool("isReobservation", isReobs),
							zap.String("guardian", obs.DelegatedGuardianAddr),
						)
						break
					}
				}
				if matchedBytes != nil {
					break
				}
			}
		}
	}

	if matchedBytes == nil {
		logger.Error("could not recover original delegate observation, skipping",
			zap.String("guardian", obs.DelegatedGuardianAddr),
			zap.String("updatedAt", obs.UpdatedAt),
		)
		return nil, fmt.Errorf("could not recover original delegate observation for guardian %s", obs.DelegatedGuardianAddr)
	}

	signed := &gossipv1.SignedDelegateObservation{
		DelegateObservation: matchedBytes,
		Signature:           sig,
		GuardianAddr:        guardianAddr.Bytes(),
	}

	envelope := &gossipv1.GossipMessage{
		Message: &gossipv1.GossipMessage_SignedDelegateObservation{
			SignedDelegateObservation: signed,
		},
	}

	return proto.Marshal(envelope)
}
