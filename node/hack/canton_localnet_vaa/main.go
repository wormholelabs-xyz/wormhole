// canton_localnet_vaa prints a VAA signed by deterministic devnet guardian keys, plus the
// guardian public-key hints that the Canton core's ParseAndVerifyVAA and SubmitGovernanceVAA take.
// For LocalNet rehearsals only: the keys are public.
//
//	go run ./hack/canton_localnet_vaa -kind guardian-set-upgrade -signing-set-index 0 -signing-key-indices 0 -new-set-index 1 -new-key-indices 1 -sequence 100
//	go run ./hack/canton_localnet_vaa -kind set-message-fee -fee 0 -sequence 101
//	go run ./hack/canton_localnet_vaa -kind transfer-fees -transfer-amount 9 -transfer-recipient <32-byte hex> -sequence 4
//	go run ./hack/canton_localnet_vaa -kind contract-upgrade -new-contract <32-byte hex> -sequence 5
//	go run ./hack/canton_localnet_vaa -kind message -signing-set-index 1 -signing-key-indices 1 -sequence 1
//	go run ./hack/canton_localnet_vaa -kind guardian-set-upgrade -signing-key-indices 1 -new-set-index 2 -new-key-indices 2,3,4 -sequence 102
//	go run ./hack/canton_localnet_vaa -kind message -signing-set-index 2 -signing-key-indices 2,3,4 -signer-positions 0,1 -sequence 2
//	go run ./hack/canton_localnet_vaa -kind ntt-transfer -signing-set-index 1 -signing-key-indices 1 -sequence 7 \
//	  -emitter-address <peer transceiver> -source-manager <peer manager> -recipient-manager <canton manager> \
//	  -source-token <peer token> -recipient-party <party id> -trimmed-decimals 8 -trimmed-amount 250000000
//	go run ./hack/canton_localnet_vaa -kind ntt-instrument-id -admin-party <party id> -instrument-nonce 0
package main

import (
	"crypto/ecdsa"
	"encoding/binary"
	"encoding/hex"
	"encoding/json"
	"flag"
	"fmt"
	"io"
	"math"
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
	vaaKindTransferFees       vaaKind = "transfer-fees"
	vaaKindContractUpgrade    vaaKind = "contract-upgrade"
	vaaKindMessage            vaaKind = "message"
	vaaKindNttTransfer        vaaKind = "ntt-transfer"
	vaaKindNttInstrumentID    vaaKind = "ntt-instrument-id"
)

// Length of the key table in node/pkg/devnet.InsecureDeterministicEcdsaKeyByIndex.
const devnetDeterministicKeyCount = 19

type kindSpecificFlag struct {
	ownerKind vaaKind
	required  bool
}

// Flags that only one kind reads.
var kindSpecificFlags = map[string]kindSpecificFlag{
	"fee":                {ownerKind: vaaKindSetMessageFee, required: true},
	"new-set-index":      {ownerKind: vaaKindGuardianSetUpgrade, required: true},
	"new-key-indices":    {ownerKind: vaaKindGuardianSetUpgrade, required: true},
	"transfer-amount":    {ownerKind: vaaKindTransferFees, required: true},
	"transfer-recipient": {ownerKind: vaaKindTransferFees, required: true},
	"new-contract":       {ownerKind: vaaKindContractUpgrade, required: true},
	"emitter-chain":      {ownerKind: vaaKindNttTransfer},
	"emitter-address":    {ownerKind: vaaKindNttTransfer, required: true},
	"source-manager":     {ownerKind: vaaKindNttTransfer, required: true},
	"recipient-manager":  {ownerKind: vaaKindNttTransfer, required: true},
	"ntt-sender":         {ownerKind: vaaKindNttTransfer},
	"ntt-message-id":     {ownerKind: vaaKindNttTransfer},
	"trimmed-decimals":   {ownerKind: vaaKindNttTransfer},
	"trimmed-amount":     {ownerKind: vaaKindNttTransfer, required: true},
	"source-token":       {ownerKind: vaaKindNttTransfer, required: true},
	"recipient-party":    {ownerKind: vaaKindNttTransfer, required: true},
	"recipient-chain":    {ownerKind: vaaKindNttTransfer},
	"admin-party":        {ownerKind: vaaKindNttInstrumentID, required: true},
	"instrument-nonce":   {ownerKind: vaaKindNttInstrumentID, required: true},
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
	// transfer-fees only.
	transferFeesAmount    uint64
	transferFeesRecipient vaa.Address
	// contract-upgrade only.
	upgradeNewContract vaa.Address
	// ntt-transfer only.
	nttEmitterChain   vaa.ChainID
	nttEmitterAddress vaa.Address
	nttTransfer       nttTransfer
	// ntt-instrument-id only.
	nttAdminParty      string
	nttInstrumentNonce uint64
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
	case vaaKindGuardianSetUpgrade, vaaKindSetMessageFee, vaaKindTransferFees, vaaKindContractUpgrade,
		vaaKindMessage, vaaKindNttTransfer, vaaKindNttInstrumentID:
		return kind, nil
	default:
		return "", fmt.Errorf("unknown kind %q (use guardian-set-upgrade, set-message-fee, transfer-fees, contract-upgrade, "+
			"message, ntt-transfer or ntt-instrument-id)", kindText)
	}
}

// nttTransferFlags holds the ntt-transfer flag values; parse them with nttTransferFromFlags.
type nttTransferFlags struct {
	emitterChain     *uint
	emitterAddress   *string
	sourceManager    *string
	recipientManager *string
	sender           *string
	messageID        *uint64
	trimmedDecimals  *uint
	trimmedAmount    *uint64
	sourceToken      *string
	recipientParty   *string
	recipientChain   *uint
}

func registerNttTransferFlags(flagSet *flag.FlagSet) nttTransferFlags {
	return nttTransferFlags{
		emitterChain:     flagSet.Uint("emitter-chain", uint(vaa.ChainIDEthereum), "ntt-transfer only: Wormhole chain id of the peer"),
		emitterAddress:   flagSet.String("emitter-address", "", "ntt-transfer only, required: peer transceiver address (32 bytes, lowercase hex)"),
		sourceManager:    flagSet.String("source-manager", "", "ntt-transfer only, required: peer manager address (32 bytes, lowercase hex)"),
		recipientManager: flagSet.String("recipient-manager", "", "ntt-transfer only, required: Canton manager address (32 bytes, lowercase hex)"),
		sender:           flagSet.String("ntt-sender", strings.Repeat("11", 32), "ntt-transfer only: sender on the peer chain (32 bytes, lowercase hex)"),
		messageID:        flagSet.Uint64("ntt-message-id", 0, "ntt-transfer only: NTT manager message id"),
		trimmedDecimals:  flagSet.Uint("trimmed-decimals", maxTrimmedDecimals, "ntt-transfer only: decimals of the trimmed amount (0 to 8)"),
		trimmedAmount:    flagSet.Uint64("trimmed-amount", 0, "ntt-transfer only, required: trimmed amount"),
		sourceToken:      flagSet.String("source-token", "", "ntt-transfer only, required: peer token address (32 bytes, lowercase hex)"),
		recipientParty:   flagSet.String("recipient-party", "", "ntt-transfer only, required: Canton recipient party id"),
		recipientChain:   flagSet.Uint("recipient-chain", uint(vaa.ChainIDCanton), "ntt-transfer only: destination Wormhole chain id"),
	}
}

func nttTransferFromFlags(flags nttTransferFlags) (vaa.ChainID, vaa.Address, nttTransfer, error) {
	transfer := nttTransfer{}
	if *flags.emitterChain == 0 || *flags.emitterChain > math.MaxUint16 {
		return 0, vaa.Address{}, transfer, fmt.Errorf("emitter chain %d is not a positive uint16", *flags.emitterChain)
	}
	if *flags.recipientChain == 0 || *flags.recipientChain > math.MaxUint16 {
		return 0, vaa.Address{}, transfer, fmt.Errorf("recipient chain %d is not a positive uint16", *flags.recipientChain)
	}
	if *flags.trimmedDecimals > maxTrimmedDecimals {
		return 0, vaa.Address{}, transfer, fmt.Errorf("trimmed decimals %d exceed %d", *flags.trimmedDecimals, maxTrimmedDecimals)
	}
	if *flags.trimmedAmount == 0 {
		return 0, vaa.Address{}, transfer, fmt.Errorf("trimmed amount must be positive")
	}
	if *flags.recipientParty == "" {
		return 0, vaa.Address{}, transfer, fmt.Errorf("recipient party is required")
	}
	emitterAddress, err := parseBytes32Hex(*flags.emitterAddress)
	if err != nil {
		return 0, vaa.Address{}, transfer, fmt.Errorf("emitter-address: %w", err)
	}
	namedAddresses := []struct {
		flagName string
		hexText  string
		target   *[32]byte
	}{
		{"source-manager", *flags.sourceManager, &transfer.SourceManager},
		{"recipient-manager", *flags.recipientManager, &transfer.RecipientManager},
		{"ntt-sender", *flags.sender, &transfer.Sender},
		{"source-token", *flags.sourceToken, &transfer.SourceToken},
	}
	for _, namedAddress := range namedAddresses {
		parsed, err := parseBytes32Hex(namedAddress.hexText)
		if err != nil {
			return 0, vaa.Address{}, transfer, fmt.Errorf("%s: %w", namedAddress.flagName, err)
		}
		*namedAddress.target = parsed
	}
	binary.BigEndian.PutUint64(transfer.MessageID[24:], *flags.messageID)
	transfer.TrimmedDecimals = uint8(*flags.trimmedDecimals) // #nosec G115 -- checked above
	transfer.TrimmedAmount = *flags.trimmedAmount
	copy(transfer.RecipientAddress[:], nttRecipientAddressForPartyText(*flags.recipientParty))
	transfer.RecipientChain = uint16(*flags.recipientChain)                             // #nosec G115 -- checked above
	return vaa.ChainID(*flags.emitterChain), vaa.Address(emitterAddress), transfer, nil // #nosec G115 -- checked above
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
	kindText := flagSet.String("kind", "", "guardian-set-upgrade, set-message-fee, transfer-fees, contract-upgrade, message, ntt-transfer or ntt-instrument-id")
	messageFee := flagSet.Uint64("fee", 0, "set-message-fee only, required: the new fee in 10^-10 fee-instrument units")
	signingSetIndexText := flagSet.String("signing-set-index", "0", "guardian set index the VAA names")
	signingKeyIndicesText := flagSet.String("signing-key-indices", "0", "deterministic key indices of the signing set, in guardian order")
	signerPositionsText := flagSet.String("signer-positions", "", "guardian positions in the signing set that sign, ascending (default: all)")
	newSetIndexText := flagSet.String("new-set-index", "", "guardian-set-upgrade only, required: the new set index")
	newKeyIndicesText := flagSet.String("new-key-indices", "", "guardian-set-upgrade only, required: deterministic key indices of the new set")
	sequence := flagSet.Uint64("sequence", 0, "VAA sequence; governance replay protection is keyed by digest")
	transferFeesAmount := flagSet.Uint64("transfer-amount", 0, "transfer-fees only, required: amount in 10^-10 fee-instrument units")
	transferFeesRecipientText := flagSet.String("transfer-recipient", "", "transfer-fees only, required: recipient (32 bytes, lowercase hex)")
	upgradeNewContractText := flagSet.String("new-contract", "", "contract-upgrade only, required: new contract (32 bytes, lowercase hex)")
	transferFlags := registerNttTransferFlags(flagSet)
	adminParty := flagSet.String("admin-party", "", "ntt-instrument-id only, required: registering admin party id")
	instrumentNonce := flagSet.Uint64("instrument-nonce", 0, "ntt-instrument-id only, required: instrument nonce")
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
		if specificFlag, kindSpecific := kindSpecificFlags[flagName]; kindSpecific && specificFlag.ownerKind != kind {
			return vaaRequest{}, fmt.Errorf("flag -%s applies only to -kind %s", flagName, specificFlag.ownerKind)
		}
	}
	for flagName, specificFlag := range kindSpecificFlags {
		if specificFlag.required && specificFlag.ownerKind == kind && !explicitFlags[flagName] {
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
	if kind == vaaKindTransferFees {
		request.transferFeesAmount = *transferFeesAmount
		recipient, err := parseBytes32Hex(*transferFeesRecipientText)
		if err != nil {
			return vaaRequest{}, fmt.Errorf("transfer-recipient: %w", err)
		}
		request.transferFeesRecipient = vaa.Address(recipient)
	}
	if kind == vaaKindContractUpgrade {
		newContract, err := parseBytes32Hex(*upgradeNewContractText)
		if err != nil {
			return vaaRequest{}, fmt.Errorf("new-contract: %w", err)
		}
		request.upgradeNewContract = vaa.Address(newContract)
	}
	if kind == vaaKindNttTransfer {
		if request.nttEmitterChain, request.nttEmitterAddress, request.nttTransfer, err = nttTransferFromFlags(transferFlags); err != nil {
			return vaaRequest{}, err
		}
	}
	if kind == vaaKindNttInstrumentID {
		if *adminParty == "" {
			return vaaRequest{}, fmt.Errorf("-admin-party must not be empty")
		}
		request.nttAdminParty = *adminParty
		request.nttInstrumentNonce = *instrumentNonce
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
	case vaaKindTransferFees:
		transfer := vaa.BodyCoreBridgeTransferFees{
			ChainID: vaa.ChainIDCanton, Amount: uint256.NewInt(request.transferFeesAmount), Recipient: request.transferFeesRecipient}
		payload, err := transfer.Serialize()
		return vaa.GovernanceChain, vaa.GovernanceEmitter, payload, err
	case vaaKindContractUpgrade:
		upgrade := vaa.BodyContractUpgrade{ChainID: vaa.ChainIDCanton, NewContract: request.upgradeNewContract}
		payload, err := upgrade.Serialize()
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
	case vaaKindNttTransfer:
		return request.nttEmitterChain, request.nttEmitterAddress, encodeWormholeTransceiverMessage(request.nttTransfer), nil
	default:
		return 0, vaa.Address{}, nil, fmt.Errorf("unknown kind %q", request.kind)
	}
}

func run() error {
	request, err := parseVAARequest(os.Args[1:])
	if err != nil {
		return err
	}
	if request.kind == vaaKindNttInstrumentID {
		return json.NewEncoder(os.Stdout).Encode(map[string]string{"instrumentId": nttInstrumentIDForAdminText(request.nttAdminParty, request.nttInstrumentNonce)})
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
