// canton_localnet_vaa prints a VAA signed by deterministic devnet guardian keys, plus the
// guardian public-key hints that the Canton core's ParseAndVerifyVAA and SubmitGovernanceVAA take.
// For LocalNet rehearsals only: the keys are public.
//
//	go run ./hack/canton_localnet_vaa -kind guardian-set-upgrade -signing-set-index 0 -signing-key-indices 0 -new-key-indices 1 -sequence 100
//	go run ./hack/canton_localnet_vaa -kind set-message-fee -fee 0 -sequence 101
//	go run ./hack/canton_localnet_vaa -kind message -signing-set-index 1 -signing-key-indices 1 -sequence 1
//	go run ./hack/canton_localnet_vaa -kind guardian-set-upgrade -signing-key-indices 1 -new-set-index 2 -new-key-indices 2,3,4 -sequence 102
//	go run ./hack/canton_localnet_vaa -kind message -signing-set-index 2 -signing-key-indices 2,3,4 -signer-positions 0,1 -sequence 2
package main

import (
	"crypto/ecdsa"
	"encoding/hex"
	"encoding/json"
	"flag"
	"fmt"
	"os"
	"strconv"
	"strings"
	"time"

	"github.com/certusone/wormhole/node/pkg/devnet"
	ethcrypto "github.com/ethereum/go-ethereum/crypto"
	"github.com/holiman/uint256"
	"github.com/wormhole-foundation/wormhole/sdk/vaa"
)

const maxGuardianSetSize = 19

// Wormhole chain id of Canton; SetMessageFee must name it.
const cantonChainID = vaa.ChainID(75)

type publicKeyHint struct {
	GuardianIndex int    `json:"guardianIndex"`
	PublicKeyHex  string `json:"publicKeyHex"`
}

type signedVAAOutput struct {
	VAAHex    string          `json:"vaaHex"`
	DigestHex string          `json:"digestHex"`
	PubKeys   []publicKeyHint `json:"pubKeys"`
}

func parseDeterministicKeyIndices(flagValue string) ([]uint64, error) {
	var keyIndices []uint64
	for _, field := range strings.Split(flagValue, ",") {
		keyIndex, err := strconv.ParseUint(strings.TrimSpace(field), 10, 8)
		if err != nil {
			return nil, fmt.Errorf("bad key index %q: %w", field, err)
		}
		keyIndices = append(keyIndices, keyIndex)
	}
	if len(keyIndices) == 0 || len(keyIndices) > maxGuardianSetSize {
		return nil, fmt.Errorf("need 1 to %d key indices, got %d", maxGuardianSetSize, len(keyIndices))
	}
	return keyIndices, nil
}

// parseSignerPositions returns the guardian positions that sign, strictly ascending and below setSize.
// An empty flag value selects every position of the set.
func parseSignerPositions(flagValue string, setSize int) ([]int, error) {
	if flagValue == "" {
		positions := make([]int, setSize)
		for position := range positions {
			positions[position] = position
		}
		return positions, nil
	}
	var positions []int
	for _, field := range strings.Split(flagValue, ",") {
		position, err := strconv.Atoi(strings.TrimSpace(field))
		if err != nil {
			return nil, fmt.Errorf("bad signer position %q: %w", field, err)
		}
		if position < 0 || position >= setSize {
			return nil, fmt.Errorf("signer position %d is outside the %d-guardian set", position, setSize)
		}
		if len(positions) > 0 && position <= positions[len(positions)-1] {
			return nil, fmt.Errorf("signer positions must be strictly ascending, got %d after %d", position, positions[len(positions)-1])
		}
		positions = append(positions, position)
	}
	return positions, nil
}

func deterministicKeys(keyIndices []uint64) []*ecdsa.PrivateKey {
	keys := make([]*ecdsa.PrivateKey, len(keyIndices))
	for position, keyIndex := range keyIndices {
		keys[position] = devnet.InsecureDeterministicEcdsaKeyByIndex(keyIndex)
	}
	return keys
}

func buildPayload(kind string, newSetIndex uint, newKeyIndicesFlag string, messageFee uint64) (vaa.ChainID, vaa.Address, []byte, error) {
	switch kind {
	case "set-message-fee":
		feeUpdate := vaa.BodyCoreBridgeSetMessageFee{ChainID: cantonChainID, MessageFee: uint256.NewInt(messageFee)}
		payload, err := feeUpdate.Serialize()
		return vaa.GovernanceChain, vaa.GovernanceEmitter, payload, err
	case "guardian-set-upgrade":
		newKeyIndices, err := parseDeterministicKeyIndices(newKeyIndicesFlag)
		if err != nil {
			return 0, vaa.Address{}, nil, err
		}
		update := vaa.BodyGuardianSetUpdate{NewIndex: uint32(newSetIndex)} // #nosec G115 -- flag value is a small set index
		for _, key := range deterministicKeys(newKeyIndices) {
			update.Keys = append(update.Keys, ethcrypto.PubkeyToAddress(key.PublicKey))
		}
		payload, err := update.Serialize()
		return vaa.GovernanceChain, vaa.GovernanceEmitter, payload, err
	case "message":
		emitterAddress := vaa.Address{31: 0x2a}
		return vaa.ChainIDEthereum, emitterAddress, []byte("canton localnet rehearsal message"), nil
	default:
		return 0, vaa.Address{}, nil, fmt.Errorf("unknown kind %q (use guardian-set-upgrade, set-message-fee or message)", kind)
	}
}

func run() error {
	kind := flag.String("kind", "", "guardian-set-upgrade, set-message-fee or message")
	messageFee := flag.Uint64("fee", 0, "set-message-fee only: the new fee in 10^-10 fee-instrument units")
	signingSetIndex := flag.Uint("signing-set-index", 0, "guardian set index the VAA names")
	signingKeyIndicesFlag := flag.String("signing-key-indices", "0", "deterministic key indices of the signing set, in guardian order")
	signerPositionsFlag := flag.String("signer-positions", "", "guardian positions in the signing set that sign, ascending (default: all)")
	newSetIndex := flag.Uint("new-set-index", 1, "guardian-set-upgrade only: the new set index")
	newKeyIndicesFlag := flag.String("new-key-indices", "1", "guardian-set-upgrade only: deterministic key indices of the new set")
	sequence := flag.Uint64("sequence", 0, "VAA sequence; governance replay protection is keyed by digest")
	flag.Parse()

	signingKeyIndices, err := parseDeterministicKeyIndices(*signingKeyIndicesFlag)
	if err != nil {
		return err
	}
	signerPositions, err := parseSignerPositions(*signerPositionsFlag, len(signingKeyIndices))
	if err != nil {
		return err
	}
	emitterChain, emitterAddress, payload, err := buildPayload(*kind, *newSetIndex, *newKeyIndicesFlag, *messageFee)
	if err != nil {
		return err
	}
	signedVAA := &vaa.VAA{
		Version:          vaa.SupportedVAAVersion,
		GuardianSetIndex: uint32(*signingSetIndex), // #nosec G115 -- flag value is a small set index
		Timestamp:        time.Now().UTC().Truncate(time.Second),
		Nonce:            0,
		Sequence:         *sequence,
		ConsistencyLevel: 0,
		EmitterChain:     emitterChain,
		EmitterAddress:   emitterAddress,
		Payload:          payload,
	}
	output := signedVAAOutput{}
	signingSetKeys := deterministicKeys(signingKeyIndices)
	for _, guardianIndex := range signerPositions {
		key := signingSetKeys[guardianIndex]
		signedVAA.AddSignature(key, uint8(guardianIndex)) // #nosec G115 -- bounded by maxGuardianSetSize
		output.PubKeys = append(output.PubKeys, publicKeyHint{
			GuardianIndex: guardianIndex,
			PublicKeyHex:  hex.EncodeToString(ethcrypto.FromECDSAPub(&key.PublicKey)),
		})
	}
	encodedVAA, err := signedVAA.Marshal()
	if err != nil {
		return err
	}
	output.VAAHex = hex.EncodeToString(encodedVAA)
	output.DigestHex = hex.EncodeToString(signedVAA.SigningDigest().Bytes())
	return json.NewEncoder(os.Stdout).Encode(output)
}

func main() {
	if err := run(); err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
}
