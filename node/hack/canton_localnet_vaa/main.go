// canton_localnet_vaa prints a VAA signed by deterministic devnet guardian keys, plus the
// guardian public-key hints that the Canton core's ParseAndVerifyVAA and SubmitGovernanceVAA take.
// For LocalNet rehearsals only: the keys are public.
//
//	go run ./hack/canton_localnet_vaa -kind guardian-set-upgrade -signing-set-index 0 -signing-key-indices 0 -new-set-index 1 -new-key-indices 1 -sequence 100
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
	"io"
	"os"
	"strconv"
	"strings"
	"time"

	"github.com/certusone/wormhole/node/pkg/common"
	"github.com/certusone/wormhole/node/pkg/devnet"
	ethcrypto "github.com/ethereum/go-ethereum/crypto"
	"github.com/holiman/uint256"
	"github.com/wormhole-foundation/wormhole/sdk/vaa"
)

type vaaKind string

const (
	vaaKindGuardianSetUpgrade vaaKind = "guardian-set-upgrade"
	vaaKindSetMessageFee      vaaKind = "set-message-fee"
	vaaKindMessage            vaaKind = "message"
)

// Length of the key table in node/pkg/devnet.InsecureDeterministicEcdsaKeyByIndex.
const devnetDeterministicKeyCount = 19

// Flags that only one kind reads.
var flagOwnerKind = map[string]vaaKind{
	"fee":             vaaKindSetMessageFee,
	"new-set-index":   vaaKindGuardianSetUpgrade,
	"new-key-indices": vaaKindGuardianSetUpgrade,
}

type vaaRequest struct {
	kind              vaaKind
	messageFee        uint64
	signingSetIndex   uint32
	signingKeyIndices []uint64
	signerPositions   []uint8
	newSetIndex       uint32
	newKeyIndices     []uint64
	sequence          uint64
}

type publicKeyHint struct {
	GuardianIndex int    `json:"guardianIndex"`
	PublicKeyHex  string `json:"publicKeyHex"`
}

type signedVAAOutput struct {
	VAAHex    string          `json:"vaaHex"`
	DigestHex string          `json:"digestHex"`
	PubKeys   []publicKeyHint `json:"pubKeys"`
}

func parseVAAKind(kindText string) (vaaKind, error) {
	switch kind := vaaKind(kindText); kind {
	case vaaKindGuardianSetUpgrade, vaaKindSetMessageFee, vaaKindMessage:
		return kind, nil
	default:
		return "", fmt.Errorf("unknown kind %q (use guardian-set-upgrade, set-message-fee or message)", kindText)
	}
}

func parseGuardianSetIndex(flagName string, indexText string) (uint32, error) {
	index, err := strconv.ParseUint(indexText, 10, 32)
	if err != nil {
		return 0, fmt.Errorf("bad -%s %q: %w", flagName, indexText, err)
	}
	return uint32(index), nil
}

// parseDeterministicKeyIndices returns 1 to common.MaxGuardianCount distinct devnet key indices.
func parseDeterministicKeyIndices(flagValue string) ([]uint64, error) {
	var keyIndices []uint64
	seenKeyIndices := make(map[uint64]bool)
	for _, field := range strings.Split(flagValue, ",") {
		keyIndex, err := strconv.ParseUint(strings.TrimSpace(field), 10, 8)
		if err != nil {
			return nil, fmt.Errorf("bad key index %q: %w", field, err)
		}
		if keyIndex >= devnetDeterministicKeyCount {
			return nil, fmt.Errorf("key index %d has no devnet key (0 to %d)", keyIndex, devnetDeterministicKeyCount-1)
		}
		if seenKeyIndices[keyIndex] {
			return nil, fmt.Errorf("duplicate key index %d", keyIndex)
		}
		seenKeyIndices[keyIndex] = true
		keyIndices = append(keyIndices, keyIndex)
	}
	if len(keyIndices) == 0 || len(keyIndices) > common.MaxGuardianCount {
		return nil, fmt.Errorf("need 1 to %d key indices, got %d", common.MaxGuardianCount, len(keyIndices))
	}
	return keyIndices, nil
}

// parseSignerPositions returns the guardian positions that sign, strictly ascending and below setSize.
// An empty flag value selects every position of the set.
func parseSignerPositions(flagValue string, setSize int) ([]uint8, error) {
	if setSize < 1 || setSize > common.MaxGuardianCount {
		return nil, fmt.Errorf("set size %d is outside 1 to %d", setSize, common.MaxGuardianCount)
	}
	if flagValue == "" {
		positions := make([]uint8, setSize)
		for position := range positions {
			positions[position] = uint8(position) // #nosec G115 -- below common.MaxGuardianCount
		}
		return positions, nil
	}
	var positions []uint8
	for _, field := range strings.Split(flagValue, ",") {
		parsedPosition, err := strconv.ParseUint(strings.TrimSpace(field), 10, 8)
		if err != nil {
			return nil, fmt.Errorf("bad signer position %q: %w", field, err)
		}
		position := uint8(parsedPosition)
		if int(position) >= setSize {
			return nil, fmt.Errorf("signer position %d is outside the %d-guardian set", position, setSize)
		}
		if len(positions) > 0 && position <= positions[len(positions)-1] {
			return nil, fmt.Errorf("signer positions must be strictly ascending, got %d after %d", position, positions[len(positions)-1])
		}
		positions = append(positions, position)
	}
	return positions, nil
}

// parseVAARequest parses the command line (without the program name) into a request for one kind.
func parseVAARequest(commandLine []string) (vaaRequest, error) {
	flagSet := flag.NewFlagSet("canton_localnet_vaa", flag.ContinueOnError)
	flagSet.SetOutput(io.Discard)
	kindText := flagSet.String("kind", "", "guardian-set-upgrade, set-message-fee or message")
	messageFee := flagSet.Uint64("fee", 0, "set-message-fee only, required: the new fee in 10^-10 fee-instrument units")
	signingSetIndexText := flagSet.String("signing-set-index", "0", "guardian set index the VAA names")
	signingKeyIndicesText := flagSet.String("signing-key-indices", "0", "deterministic key indices of the signing set, in guardian order")
	signerPositionsText := flagSet.String("signer-positions", "", "guardian positions in the signing set that sign, ascending (default: all)")
	newSetIndexText := flagSet.String("new-set-index", "", "guardian-set-upgrade only, required: the new set index")
	newKeyIndicesText := flagSet.String("new-key-indices", "", "guardian-set-upgrade only, required: deterministic key indices of the new set")
	sequence := flagSet.Uint64("sequence", 0, "VAA sequence; governance replay protection is keyed by digest")
	if err := flagSet.Parse(commandLine); err != nil {
		return vaaRequest{}, err
	}
	if flagSet.NArg() != 0 {
		return vaaRequest{}, fmt.Errorf("unexpected argument %q", flagSet.Arg(0))
	}
	kind, err := parseVAAKind(*kindText)
	if err != nil {
		return vaaRequest{}, err
	}
	explicitFlags := make(map[string]bool)
	flagSet.Visit(func(setFlag *flag.Flag) { explicitFlags[setFlag.Name] = true })
	for flagName := range explicitFlags {
		if ownerKind, kindSpecific := flagOwnerKind[flagName]; kindSpecific && ownerKind != kind {
			return vaaRequest{}, fmt.Errorf("flag -%s applies only to -kind %s", flagName, ownerKind)
		}
	}
	for flagName, ownerKind := range flagOwnerKind {
		if ownerKind == kind && !explicitFlags[flagName] {
			return vaaRequest{}, fmt.Errorf("-kind %s requires -%s", kind, flagName)
		}
	}

	request := vaaRequest{kind: kind, messageFee: *messageFee, sequence: *sequence}
	if request.signingSetIndex, err = parseGuardianSetIndex("signing-set-index", *signingSetIndexText); err != nil {
		return vaaRequest{}, err
	}
	if request.signingKeyIndices, err = parseDeterministicKeyIndices(*signingKeyIndicesText); err != nil {
		return vaaRequest{}, err
	}
	if request.signerPositions, err = parseSignerPositions(*signerPositionsText, len(request.signingKeyIndices)); err != nil {
		return vaaRequest{}, err
	}
	if kind == vaaKindGuardianSetUpgrade {
		if request.newSetIndex, err = parseGuardianSetIndex("new-set-index", *newSetIndexText); err != nil {
			return vaaRequest{}, err
		}
		if request.newKeyIndices, err = parseDeterministicKeyIndices(*newKeyIndicesText); err != nil {
			return vaaRequest{}, err
		}
	}
	return request, nil
}

func deterministicKeys(keyIndices []uint64) []*ecdsa.PrivateKey {
	keys := make([]*ecdsa.PrivateKey, len(keyIndices))
	for position, keyIndex := range keyIndices {
		keys[position] = devnet.InsecureDeterministicEcdsaKeyByIndex(keyIndex)
	}
	return keys
}

func buildPayload(request vaaRequest) (vaa.ChainID, vaa.Address, []byte, error) {
	switch request.kind {
	case vaaKindSetMessageFee:
		feeUpdate := vaa.BodyCoreBridgeSetMessageFee{ChainID: vaa.ChainIDCanton, MessageFee: uint256.NewInt(request.messageFee)}
		payload, err := feeUpdate.Serialize()
		return vaa.GovernanceChain, vaa.GovernanceEmitter, payload, err
	case vaaKindGuardianSetUpgrade:
		if len(request.newKeyIndices) == 0 {
			return 0, vaa.Address{}, nil, fmt.Errorf("guardian-set-upgrade needs new key indices")
		}
		update := vaa.BodyGuardianSetUpdate{NewIndex: request.newSetIndex}
		for _, key := range deterministicKeys(request.newKeyIndices) {
			update.Keys = append(update.Keys, ethcrypto.PubkeyToAddress(key.PublicKey))
		}
		payload, err := update.Serialize()
		return vaa.GovernanceChain, vaa.GovernanceEmitter, payload, err
	case vaaKindMessage:
		emitterAddress := vaa.Address{31: 0x2a}
		return vaa.ChainIDEthereum, emitterAddress, []byte("canton localnet rehearsal message"), nil
	default:
		return 0, vaa.Address{}, nil, fmt.Errorf("unknown kind %q", request.kind)
	}
}

func run() error {
	request, err := parseVAARequest(os.Args[1:])
	if err != nil {
		return err
	}
	emitterChain, emitterAddress, payload, err := buildPayload(request)
	if err != nil {
		return err
	}
	signedVAA := &vaa.VAA{
		Version:          vaa.SupportedVAAVersion,
		GuardianSetIndex: request.signingSetIndex,
		Timestamp:        time.Now().UTC().Truncate(time.Second),
		Nonce:            0,
		Sequence:         request.sequence,
		ConsistencyLevel: 0,
		EmitterChain:     emitterChain,
		EmitterAddress:   emitterAddress,
		Payload:          payload,
	}
	output := signedVAAOutput{}
	signingSetKeys := deterministicKeys(request.signingKeyIndices)
	for _, guardianIndex := range request.signerPositions {
		key := signingSetKeys[guardianIndex]
		signedVAA.AddSignature(key, guardianIndex)
		output.PubKeys = append(output.PubKeys, publicKeyHint{
			GuardianIndex: int(guardianIndex),
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
