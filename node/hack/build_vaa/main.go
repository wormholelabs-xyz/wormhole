// This tool builds a VAA from observation signatures fetched from the
// wormholescan API. It fetches the full message data from an EVM RPC node
// (using the chain config for contract addresses and default RPCs), fetches
// the current guardian set from Ethereum to map guardian addresses to indices,
// and assembles the VAA.
//
// By default it logs the resulting VAA as 0x-prefixed hex. If the -broadcast
// flag is passed, it also broadcasts the VAA over the p2p gossip network.
//
// Usage:
//
//	go run main.go \
//	  -vaaID 59/0000000000000000000000005d4c6f2235d508b1e5a324c078b66cda2f5e7d5d/5942 \
//	  -network mainnet
package main

import (
	"context"
	"encoding/base64"
	"encoding/hex"
	"encoding/json"
	"flag"
	"fmt"
	"io"
	"net/http"
	"os"
	"sort"
	"strconv"
	"strings"
	"time"

	"github.com/certusone/wormhole/node/pkg/common"
	"github.com/certusone/wormhole/node/pkg/p2p"
	gossipv1 "github.com/certusone/wormhole/node/pkg/proto/gossip/v1"
	"github.com/certusone/wormhole/node/pkg/watchers/evm"
	"github.com/certusone/wormhole/node/pkg/watchers/evm/connectors"
	ethAbi "github.com/certusone/wormhole/node/pkg/watchers/evm/connectors/ethabi"
	ethBind "github.com/ethereum/go-ethereum/accounts/abi/bind"
	ethCommon "github.com/ethereum/go-ethereum/common"
	ethcrypto "github.com/ethereum/go-ethereum/crypto"
	ethClient "github.com/ethereum/go-ethereum/ethclient"
	ethRpc "github.com/ethereum/go-ethereum/rpc"
	pubsub "github.com/libp2p/go-libp2p-pubsub"
	p2pcrypto "github.com/libp2p/go-libp2p/core/crypto"
	"github.com/wormhole-foundation/wormhole/sdk/vaa"
	"go.uber.org/zap"
	"google.golang.org/protobuf/proto"
)

// Use a dedicated FlagSet to avoid glog's vmodule flag panic on flag.Parse().
var fs = flag.NewFlagSet(os.Args[0], flag.ExitOnError)

var (
	vaaIDFlag      = fs.String("vaaID", "", "VAA ID in chain/emitter/sequence format (e.g. 59/0000000000000000000000005d4c6f2235d508b1e5a324c078b66cda2f5e7d5d/5942)")
	ethRPCFlag     = fs.String("ethRPC", "", "EVM RPC endpoint URL (defaults to the PublicRPC from chain config)")
	ethContract    = fs.String("ethContract", "", "Wormhole core bridge contract address (defaults to chain config)")
	gsEthRPC       = fs.String("gsEthRPC", "", "Ethereum RPC for fetching the guardian set (defaults to Ethereum PublicRPC from chain config)")
	gsContract     = fs.String("gsContract", "", "Ethereum core bridge contract for guardian set (defaults to Ethereum ContractAddr from chain config)")
	nodeKey        = fs.String("nodeKey", "/tmp/build_vaa.key", "Path to p2p node key file")
	network        = fs.String("network", "mainnet", "Network: mainnet, testnet, devnet")
	bootstrapPeers = fs.String("bootstrapPeers", "", "Override bootstrap peers (comma-separated multiaddrs)")
	apiURL         = fs.String("apiURL", "https://api.wormholescan.io", "Wormholescan API base URL")
	p2pPort        = fs.Uint("port", 8999, "P2P listen port")
	broadcast      = fs.Bool("broadcast", false, "Broadcast the VAA over gossip")
)

// observationAPIResponse represents a single entry from the wormholescan observations API.
type observationAPIResponse struct {
	Sequence    uint64 `json:"sequence"`
	ID          string `json:"id"`
	EmitterChain uint32 `json:"emitterChain"`
	EmitterAddr string `json:"emitterAddr"`
	Hash        string `json:"hash"`
	TxHash      string `json:"txHash"`
	GuardianAddr string `json:"guardianAddr"`
	Signature   string `json:"signature"`
	UpdatedAt   string `json:"updatedAt"`
	IndexedAt   string `json:"indexedAt"`
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
	rpcURL := *ethRPCFlag
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

	// Fetch observations from wormholescan.
	observations, err := fetchObservations(*apiURL, chainID, emitter, sequence)
	if err != nil {
		logger.Fatal("failed to fetch observations", zap.Error(err))
	}
	logger.Info("fetched observations", zap.Int("count", len(observations)))

	if len(observations) == 0 {
		logger.Fatal("no observations found for this VAA ID")
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

	// Fetch the current guardian set.
	gsIndex, gs, err := fetchCurrentGuardianSet(ctx, env)
	if err != nil {
		logger.Fatal("failed to fetch current guardian set", zap.Error(err))
	}
	logger.Info("fetched guardian set", zap.Uint32("index", gsIndex), zap.Int("numGuardians", len(gs.Keys)))

	// Build guardian address -> index map.
	guardianIndexMap := make(map[ethCommon.Address]uint8)
	for i, key := range gs.Keys {
		guardianIndexMap[key] = uint8(i) // #nosec G115 -- guardian set is always < 256
	}

	// Compute the expected signing digest from the message.
	v := &vaa.VAA{
		Version:          1,
		GuardianSetIndex: gsIndex,
		Timestamp:        msgPub.Timestamp,
		Nonce:            msgPub.Nonce,
		Sequence:         msgPub.Sequence,
		ConsistencyLevel: msgPub.ConsistencyLevel,
		EmitterChain:     msgPub.EmitterChain,
		EmitterAddress:   msgPub.EmitterAddress,
		Payload:          msgPub.Payload,
	}
	digest := v.SigningDigest()
	logger.Info("computed signing digest", zap.String("digest", hex.EncodeToString(digest.Bytes())))

	// Map observation signatures to VAA signatures.
	var sigs []*vaa.Signature
	for _, obs := range observations {
		guardianAddr := ethCommon.HexToAddress(strings.TrimPrefix(obs.GuardianAddr, "0x"))

		idx, ok := guardianIndexMap[guardianAddr]
		if !ok {
			logger.Warn("guardian not in current set, skipping",
				zap.String("guardian", obs.GuardianAddr))
			continue
		}

		sigBytes, err := base64.StdEncoding.DecodeString(obs.Signature)
		if err != nil {
			logger.Warn("failed to decode signature, skipping",
				zap.String("guardian", obs.GuardianAddr), zap.Error(err))
			continue
		}

		if len(sigBytes) != 65 {
			logger.Warn("unexpected signature length, skipping",
				zap.String("guardian", obs.GuardianAddr), zap.Int("len", len(sigBytes)))
			continue
		}

		// Verify the signature matches the digest.
		recoveredPubKey, err := ethcrypto.Ecrecover(digest.Bytes(), sigBytes)
		if err != nil {
			logger.Warn("failed to recover public key from signature, skipping",
				zap.String("guardian", obs.GuardianAddr), zap.Error(err))
			continue
		}
		recoveredAddr := ethCommon.BytesToAddress(ethcrypto.Keccak256(recoveredPubKey[1:])[12:])
		if recoveredAddr != guardianAddr {
			logger.Warn("signature does not match guardian address, skipping",
				zap.String("guardian", obs.GuardianAddr),
				zap.String("recovered", recoveredAddr.Hex()))
			continue
		}

		var sigData vaa.SignatureData
		copy(sigData[:], sigBytes)

		sigs = append(sigs, &vaa.Signature{
			Index:     idx,
			Signature: sigData,
		})

		logger.Info("added signature",
			zap.Uint8("guardianIndex", idx),
			zap.String("guardian", obs.GuardianAddr))
	}

	// Sort signatures by guardian index (required by VAA spec).
	sort.Slice(sigs, func(i, j int) bool {
		return sigs[i].Index < sigs[j].Index
	})
	v.Signatures = sigs

	quorum := len(gs.Keys)*2/3 + 1
	logger.Info("signature summary",
		zap.Int("collected", len(sigs)),
		zap.Int("quorum", quorum),
		zap.Int("guardians", len(gs.Keys)),
	)

	if len(sigs) < quorum {
		logger.Warn("insufficient signatures for quorum")
	}

	// Serialize the VAA.
	vaaBytes, err := v.Marshal()
	if err != nil {
		logger.Fatal("failed to marshal VAA", zap.Error(err))
	}

	fmt.Printf("0x%s\n", hex.EncodeToString(vaaBytes))

	if !*broadcast {
		return
	}

	// Broadcast the VAA over gossip.
	logger.Info("broadcasting VAA over gossip...")

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

	topic := fmt.Sprintf("%s/%s", networkID, "broadcast")
	logger.Info("joining topic", zap.String("topic", topic))

	th, err := ps.Join(topic)
	if err != nil {
		logger.Fatal("failed to join broadcast topic", zap.Error(err))
	}
	defer th.Close()

	// Wait for peers.
	logger.Info("waiting for peers...", zap.String("peer_id", h.ID().String()))
	timeout := time.After(60 * time.Second)
	for len(th.ListPeers()) < 3 {
		select {
		case <-timeout:
			logger.Fatal("timed out waiting for peers")
		default:
			time.Sleep(100 * time.Millisecond)
		}
	}
	logger.Info("connected to peers", zap.Int("count", len(th.ListPeers())))

	// Build and publish the gossip message.
	envelope := &gossipv1.GossipMessage{
		Message: &gossipv1.GossipMessage_SignedVaaWithQuorum{
			SignedVaaWithQuorum: &gossipv1.SignedVAAWithQuorum{Vaa: vaaBytes},
		},
	}

	msg, err := proto.Marshal(envelope)
	if err != nil {
		logger.Fatal("failed to marshal gossip message", zap.Error(err))
	}

	if err := th.Publish(ctx, msg); err != nil {
		logger.Fatal("failed to publish VAA", zap.Error(err))
	}

	logger.Info("published VAA to gossip network")
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

// fetchObservations calls the wormholescan API to get observations for a VAA ID.
func fetchObservations(baseURL string, chain uint16, emitter string, sequence uint64) ([]observationAPIResponse, error) {
	url := fmt.Sprintf("%s/api/v1/observations/%d/%s/%d", baseURL, chain, emitter, sequence)

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

	var observations []observationAPIResponse
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

// fetchCurrentGuardianSet fetches the current guardian set from the Ethereum core bridge contract.
// It uses the chain config for the Ethereum RPC and contract address unless overridden via flags.
func fetchCurrentGuardianSet(ctx context.Context, env common.Environment) (uint32, *ethAbi.StructsGuardianSet, error) {
	rpcURL := *gsEthRPC
	contractAddrStr := *gsContract

	if rpcURL == "" || contractAddrStr == "" {
		chainConfig, err := evm.GetChainConfigMap(env)
		if err != nil {
			return 0, nil, fmt.Errorf("failed to get chain config: %w", err)
		}
		ethEntry, ok := chainConfig[vaa.ChainIDEthereum]
		if !ok {
			return 0, nil, fmt.Errorf("no Ethereum entry in chain config for network %s", env)
		}
		if rpcURL == "" {
			if ethEntry.PublicRPC == "" {
				return 0, nil, fmt.Errorf("no Ethereum PublicRPC in chain config (use -gsEthRPC to specify)")
			}
			rpcURL = ethEntry.PublicRPC
		}
		if contractAddrStr == "" {
			if ethEntry.ContractAddr == "" {
				return 0, nil, fmt.Errorf("no Ethereum ContractAddr in chain config (use -gsContract to specify)")
			}
			contractAddrStr = ethEntry.ContractAddr
		}
	}

	contract := ethCommon.HexToAddress(contractAddrStr)
	rawClient, err := ethRpc.DialContext(ctx, rpcURL)
	if err != nil {
		return 0, nil, fmt.Errorf("failed to connect to Ethereum: %w", err)
	}
	client := ethClient.NewClient(rawClient)

	caller, err := ethAbi.NewAbiCaller(contract, client)
	if err != nil {
		return 0, nil, fmt.Errorf("failed to create ABI caller: %w", err)
	}

	currentIndex, err := caller.GetCurrentGuardianSetIndex(&ethBind.CallOpts{Context: ctx})
	if err != nil {
		return 0, nil, fmt.Errorf("error requesting current guardian set index: %w", err)
	}

	gs, err := caller.GetGuardianSet(&ethBind.CallOpts{Context: ctx}, currentIndex)
	if err != nil {
		return 0, nil, fmt.Errorf("error requesting current guardian set: %w", err)
	}

	return currentIndex, &gs, nil
}
