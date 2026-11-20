package main

import (
	"bytes"
	"encoding/binary"
	"encoding/hex"
	"math"
	"reflect"
	"strconv"
	"strings"
	"testing"

	"github.com/certusone/wormhole/node/pkg/common"
	"github.com/certusone/wormhole/node/pkg/devnet"
	ethcommon "github.com/ethereum/go-ethereum/common"
	ethcrypto "github.com/ethereum/go-ethereum/crypto"
	"github.com/holiman/uint256"
	"github.com/wormhole-foundation/wormhole/sdk/vaa"
)

func TestParseSignerPositions(t *testing.T) {
	testCases := []struct {
		name              string
		flagValue         string
		setSize           int
		expectedPositions []uint8
		expectError       bool
	}{
		{name: "empty selects all", flagValue: "", setSize: 3, expectedPositions: []uint8{0, 1, 2}},
		{name: "subset", flagValue: "0,1", setSize: 3, expectedPositions: []uint8{0, 1}},
		{name: "last position", flagValue: "2", setSize: 3, expectedPositions: []uint8{2}},
		{name: "descending", flagValue: "1,0", setSize: 3, expectError: true},
		{name: "duplicate", flagValue: "1,1", setSize: 3, expectError: true},
		{name: "equal to set size", flagValue: "3", setSize: 3, expectError: true},
		{name: "negative", flagValue: "-1", setSize: 3, expectError: true},
		{name: "above uint8", flagValue: "256", setSize: 3, expectError: true},
		{name: "not a number", flagValue: "a", setSize: 3, expectError: true},
		{name: "empty field", flagValue: "0,", setSize: 3, expectError: true},
	}
	for _, testCase := range testCases {
		t.Run(testCase.name, func(t *testing.T) {
			positions, err := parseSignerPositions(testCase.flagValue, testCase.setSize)
			if testCase.expectError {
				if err == nil {
					t.Fatalf("expected an error, got positions %v", positions)
				}
				return
			}
			if err != nil {
				t.Fatalf("unexpected error: %v", err)
			}
			if !reflect.DeepEqual(positions, testCase.expectedPositions) {
				t.Fatalf("positions %v, expected %v", positions, testCase.expectedPositions)
			}
		})
	}
}

func joinKeyIndices(keyIndices []uint64) string {
	fields := make([]string, len(keyIndices))
	for position, keyIndex := range keyIndices {
		fields[position] = strconv.FormatUint(keyIndex, 10)
	}
	return strings.Join(fields, ",")
}

func TestParseDeterministicKeyIndices(t *testing.T) {
	allDevnetKeyIndices := make([]uint64, devnetDeterministicKeyCount)
	for keyIndex := range allDevnetKeyIndices {
		allDevnetKeyIndices[keyIndex] = uint64(keyIndex) // #nosec G115 -- below devnetDeterministicKeyCount
	}
	testCases := []struct {
		name               string
		flagValue          string
		expectedKeyIndices []uint64
		expectError        bool
	}{
		{name: "one key", flagValue: "0", expectedKeyIndices: []uint64{0}},
		{name: "three keys", flagValue: "2,3,4", expectedKeyIndices: []uint64{2, 3, 4}},
		{name: "last devnet key", flagValue: "18", expectedKeyIndices: []uint64{18}},
		{name: "every devnet key", flagValue: joinKeyIndices(allDevnetKeyIndices), expectedKeyIndices: allDevnetKeyIndices},
		{name: "empty", flagValue: "", expectError: true},
		{name: "duplicate", flagValue: "1,1", expectError: true},
		{name: "no such devnet key", flagValue: "19", expectError: true},
		{name: "above uint8", flagValue: "256", expectError: true},
		{name: "negative", flagValue: "-1", expectError: true},
		{name: "not a number", flagValue: "a", expectError: true},
		{name: "empty field", flagValue: "0,", expectError: true},
		{name: "above the guardian count", flagValue: joinKeyIndices(append(append([]uint64{}, allDevnetKeyIndices...), 19)), expectError: true},
	}
	for _, testCase := range testCases {
		t.Run(testCase.name, func(t *testing.T) {
			keyIndices, err := parseDeterministicKeyIndices(testCase.flagValue)
			if testCase.expectError {
				if err == nil {
					t.Fatalf("expected an error, got key indices %v", keyIndices)
				}
				return
			}
			if err != nil {
				t.Fatalf("unexpected error: %v", err)
			}
			if !reflect.DeepEqual(keyIndices, testCase.expectedKeyIndices) {
				t.Fatalf("key indices %v, expected %v", keyIndices, testCase.expectedKeyIndices)
			}
		})
	}
}

func TestDevnetDeterministicKeyCountIsTheGuardianCount(t *testing.T) {
	if devnetDeterministicKeyCount != common.MaxGuardianCount {
		t.Fatalf("devnetDeterministicKeyCount %d, common.MaxGuardianCount %d", devnetDeterministicKeyCount, common.MaxGuardianCount)
	}
}

func TestParseGuardianSetIndex(t *testing.T) {
	testCases := []struct {
		name          string
		indexText     string
		expectedIndex uint32
		expectError   bool
	}{
		{name: "zero", indexText: "0", expectedIndex: 0},
		{name: "max uint32", indexText: "4294967295", expectedIndex: math.MaxUint32},
		{name: "max uint32 + 1", indexText: "4294967296", expectError: true},
		{name: "wraps to 1 as a uint cast", indexText: "4294967297", expectError: true},
		{name: "negative", indexText: "-1", expectError: true},
		{name: "empty", indexText: "", expectError: true},
		{name: "exponent", indexText: "1e3", expectError: true},
	}
	for _, testCase := range testCases {
		t.Run(testCase.name, func(t *testing.T) {
			index, err := parseGuardianSetIndex("signing-set-index", testCase.indexText)
			if testCase.expectError {
				if err == nil {
					t.Fatalf("expected an error, got index %d", index)
				}
				return
			}
			if err != nil {
				t.Fatalf("unexpected error: %v", err)
			}
			if index != testCase.expectedIndex {
				t.Fatalf("index %d, expected %d", index, testCase.expectedIndex)
			}
		})
	}
}

func TestParseVAARequest(t *testing.T) {
	testCases := []struct {
		name                 string
		commandLine          string
		expectedRequest      vaaRequest
		expectedErrorMessage string
	}{
		{
			name:        "guardian-set-upgrade, one key",
			commandLine: "-kind guardian-set-upgrade -signing-set-index 0 -signing-key-indices 0 -new-set-index 1 -new-key-indices 1 -sequence 100",
			expectedRequest: vaaRequest{kind: vaaKindGuardianSetUpgrade, signingSetIndex: 0, signingKeyIndices: []uint64{0},
				signerPositions: []uint8{0}, newSetIndex: 1, newKeyIndices: []uint64{1}, sequence: 100},
		},
		{
			name:        "set-message-fee with defaults",
			commandLine: "-kind set-message-fee -fee 0 -sequence 101",
			expectedRequest: vaaRequest{kind: vaaKindSetMessageFee, messageFee: 0, signingSetIndex: 0, signingKeyIndices: []uint64{0},
				signerPositions: []uint8{0}, sequence: 101},
		},
		{
			name:        "set-message-fee, quorum script shape",
			commandLine: "-kind set-message-fee -fee 1000 -signing-set-index 2 -signing-key-indices 2,3,4 -sequence 1700000000",
			expectedRequest: vaaRequest{kind: vaaKindSetMessageFee, messageFee: 1000, signingSetIndex: 2, signingKeyIndices: []uint64{2, 3, 4},
				signerPositions: []uint8{0, 1, 2}, sequence: 1700000000},
		},
		{
			name:        "message",
			commandLine: "-kind message -signing-set-index 1 -signing-key-indices 1 -sequence 1",
			expectedRequest: vaaRequest{kind: vaaKindMessage, signingSetIndex: 1, signingKeyIndices: []uint64{1},
				signerPositions: []uint8{0}, sequence: 1},
		},
		{
			name:        "guardian-set-upgrade, three keys",
			commandLine: "-kind guardian-set-upgrade -signing-set-index 1 -signing-key-indices 1 -new-set-index 2 -new-key-indices 2,3,4 -sequence 102",
			expectedRequest: vaaRequest{kind: vaaKindGuardianSetUpgrade, signingSetIndex: 1, signingKeyIndices: []uint64{1},
				signerPositions: []uint8{0}, newSetIndex: 2, newKeyIndices: []uint64{2, 3, 4}, sequence: 102},
		},
		{
			name:        "message signed by a subset",
			commandLine: "-kind message -signing-set-index 2 -signing-key-indices 2,3,4 -signer-positions 0,1 -sequence 2",
			expectedRequest: vaaRequest{kind: vaaKindMessage, signingSetIndex: 2, signingKeyIndices: []uint64{2, 3, 4},
				signerPositions: []uint8{0, 1}, sequence: 2},
		},
		{
			name:        "message at the max set index",
			commandLine: "-kind message -signing-set-index 4294967295",
			expectedRequest: vaaRequest{kind: vaaKindMessage, signingSetIndex: math.MaxUint32, signingKeyIndices: []uint64{0},
				signerPositions: []uint8{0}},
		},
		{
			name:        "transfer-fees",
			commandLine: "-kind transfer-fees -transfer-amount 9 -transfer-recipient " + strings.Repeat("d4", 32) + " -sequence 4",
			expectedRequest: vaaRequest{kind: vaaKindTransferFees, signingSetIndex: 0, signingKeyIndices: []uint64{0},
				signerPositions: []uint8{0}, sequence: 4, transferFeesAmount: 9, transferFeesRecipient: vaa.Address(bytes.Repeat([]byte{0xd4}, 32))},
		},
		{name: "transfer-fees without recipient", commandLine: "-kind transfer-fees -transfer-amount 9",
			expectedErrorMessage: "-kind transfer-fees requires -transfer-recipient"},
		{name: "transfer-fees with a 31-byte recipient", commandLine: "-kind transfer-fees -transfer-amount 9 -transfer-recipient " + strings.Repeat("d4", 31),
			expectedErrorMessage: "32 bytes"},
		{name: "transfer amount on set-message-fee", commandLine: "-kind set-message-fee -fee 1 -transfer-amount 9",
			expectedErrorMessage: "flag -transfer-amount applies only to -kind transfer-fees"},
		{
			name:        "contract-upgrade",
			commandLine: "-kind contract-upgrade -new-contract " + strings.Repeat("c1", 32) + " -sequence 5",
			expectedRequest: vaaRequest{kind: vaaKindContractUpgrade, signingSetIndex: 0, signingKeyIndices: []uint64{0},
				signerPositions: []uint8{0}, sequence: 5, upgradeNewContract: vaa.Address(bytes.Repeat([]byte{0xc1}, 32))},
		},
		{name: "contract-upgrade without new contract", commandLine: "-kind contract-upgrade",
			expectedErrorMessage: "-kind contract-upgrade requires -new-contract"},
		{name: "fee on message", commandLine: "-kind message -fee 5", expectedErrorMessage: "flag -fee applies only to -kind set-message-fee"},
		{name: "new keys on set-message-fee", commandLine: "-kind set-message-fee -fee 1 -new-key-indices 1",
			expectedErrorMessage: "flag -new-key-indices applies only to -kind guardian-set-upgrade"},
		{name: "new set index on message", commandLine: "-kind message -new-set-index 2",
			expectedErrorMessage: "flag -new-set-index applies only to -kind guardian-set-upgrade"},
		{name: "set-message-fee without fee", commandLine: "-kind set-message-fee", expectedErrorMessage: "-kind set-message-fee requires -fee"},
		{name: "guardian-set-upgrade without new keys", commandLine: "-kind guardian-set-upgrade -new-set-index 1",
			expectedErrorMessage: "-kind guardian-set-upgrade requires -new-key-indices"},
		{name: "guardian-set-upgrade without new set index", commandLine: "-kind guardian-set-upgrade -new-key-indices 1",
			expectedErrorMessage: "-kind guardian-set-upgrade requires -new-set-index"},
		{name: "unknown kind", commandLine: "-kind bogus", expectedErrorMessage: `unknown kind "bogus"`},
		{name: "no kind", commandLine: "-sequence 1", expectedErrorMessage: `unknown kind ""`},
		{name: "signing set index above uint32", commandLine: "-kind message -signing-set-index 4294967297",
			expectedErrorMessage: "-signing-set-index"},
		{name: "new set index above uint32", commandLine: "-kind guardian-set-upgrade -new-set-index 4294967296 -new-key-indices 1",
			expectedErrorMessage: "-new-set-index"},
		{name: "duplicate signing key", commandLine: "-kind message -signing-key-indices 1,1", expectedErrorMessage: "duplicate key index 1"},
		{name: "duplicate new key", commandLine: "-kind guardian-set-upgrade -new-set-index 1 -new-key-indices 2,2",
			expectedErrorMessage: "duplicate key index 2"},
		{name: "signer position outside the set", commandLine: "-kind message -signing-key-indices 1 -signer-positions 1",
			expectedErrorMessage: "signer position 1 is outside the 1-guardian set"},
		{name: "positional argument", commandLine: "-kind message extra", expectedErrorMessage: "unexpected argument"},
		{name: "unknown flag", commandLine: "-kind message -bogus 1", expectedErrorMessage: "flag provided but not defined"},
	}
	for _, testCase := range testCases {
		t.Run(testCase.name, func(t *testing.T) {
			request, err := parseVAARequest(strings.Fields(testCase.commandLine))
			if testCase.expectedErrorMessage != "" {
				if err == nil {
					t.Fatalf("expected an error with %q, got request %+v", testCase.expectedErrorMessage, request)
				}
				if !strings.Contains(err.Error(), testCase.expectedErrorMessage) {
					t.Fatalf("error %q does not contain %q", err.Error(), testCase.expectedErrorMessage)
				}
				return
			}
			if err != nil {
				t.Fatalf("unexpected error: %v", err)
			}
			if !reflect.DeepEqual(request, testCase.expectedRequest) {
				t.Fatalf("request %+v, expected %+v", request, testCase.expectedRequest)
			}
		})
	}
}

func TestBuildPayload(t *testing.T) {
	expectedFeePayload, err := vaa.BodyCoreBridgeSetMessageFee{ChainID: vaa.ChainIDCanton, MessageFee: uint256.NewInt(1000)}.Serialize()
	if err != nil {
		t.Fatalf("serialize the expected fee payload: %v", err)
	}
	transferFeesRecipient := vaa.Address(bytes.Repeat([]byte{0xd4}, 32))
	expectedTransferFeesPayload, err := vaa.BodyCoreBridgeTransferFees{
		ChainID: vaa.ChainIDCanton, Amount: uint256.NewInt(9), Recipient: transferFeesRecipient}.Serialize()
	if err != nil {
		t.Fatalf("serialize the expected transfer-fees payload: %v", err)
	}
	upgradeNewContract := vaa.Address(bytes.Repeat([]byte{0xc1}, 32))
	expectedContractUpgradePayload, err := vaa.BodyContractUpgrade{ChainID: vaa.ChainIDCanton, NewContract: upgradeNewContract}.Serialize()
	if err != nil {
		t.Fatalf("serialize the expected contract-upgrade payload: %v", err)
	}
	testCases := []struct {
		name                   string
		request                vaaRequest
		expectedEmitterChain   vaa.ChainID
		expectedEmitterAddress vaa.Address
		checkPayload           func(t *testing.T, payload []byte)
	}{
		{
			name:                   "set-message-fee names Canton",
			request:                vaaRequest{kind: vaaKindSetMessageFee, messageFee: 1000},
			expectedEmitterChain:   vaa.GovernanceChain,
			expectedEmitterAddress: vaa.GovernanceEmitter,
			checkPayload: func(t *testing.T, payload []byte) {
				if !bytes.Equal(payload, expectedFeePayload) {
					t.Fatalf("payload %x, expected %x", payload, expectedFeePayload)
				}
				if !bytes.Equal(payload[33:35], []byte{0x00, 0x4b}) {
					t.Fatalf("target chain bytes %x, expected 004b", payload[33:35])
				}
				if vaa.ChainID(binary.BigEndian.Uint16(payload[33:35])) != vaa.ChainIDCanton {
					t.Fatalf("target chain is not vaa.ChainIDCanton")
				}
			},
		},
		{
			name:                   "guardian-set-upgrade lists the new keys in order",
			request:                vaaRequest{kind: vaaKindGuardianSetUpgrade, newSetIndex: 2, newKeyIndices: []uint64{2, 3, 4}},
			expectedEmitterChain:   vaa.GovernanceChain,
			expectedEmitterAddress: vaa.GovernanceEmitter,
			checkPayload: func(t *testing.T, payload []byte) {
				const keyCountOffset = 39
				const addressLength = 20
				if len(payload) != keyCountOffset+1+3*addressLength {
					t.Fatalf("payload length %d, expected %d", len(payload), keyCountOffset+1+3*addressLength)
				}
				if binary.BigEndian.Uint32(payload[35:39]) != 2 {
					t.Fatalf("new set index %d, expected 2", binary.BigEndian.Uint32(payload[35:39]))
				}
				if payload[keyCountOffset] != 3 {
					t.Fatalf("key count %d, expected 3", payload[keyCountOffset])
				}
				for position, keyIndex := range []uint64{2, 3, 4} {
					expectedAddress := ethcrypto.PubkeyToAddress(devnet.InsecureDeterministicEcdsaKeyByIndex(keyIndex).PublicKey)
					addressStart := keyCountOffset + 1 + position*addressLength
					if !bytes.Equal(payload[addressStart:addressStart+addressLength], expectedAddress.Bytes()) {
						t.Fatalf("address %d is %x, expected %x", position, payload[addressStart:addressStart+addressLength], expectedAddress.Bytes())
					}
				}
			},
		},
		{
			name:                   "transfer-fees names Canton",
			request:                vaaRequest{kind: vaaKindTransferFees, transferFeesAmount: 9, transferFeesRecipient: transferFeesRecipient},
			expectedEmitterChain:   vaa.GovernanceChain,
			expectedEmitterAddress: vaa.GovernanceEmitter,
			checkPayload: func(t *testing.T, payload []byte) {
				const transferFeesPayloadLength = 99
				if len(payload) != transferFeesPayloadLength {
					t.Fatalf("payload length %d, expected %d", len(payload), transferFeesPayloadLength)
				}
				if !bytes.Equal(payload, expectedTransferFeesPayload) {
					t.Fatalf("payload %x, expected %x", payload, expectedTransferFeesPayload)
				}
			},
		},
		{
			name:                   "contract-upgrade names Canton",
			request:                vaaRequest{kind: vaaKindContractUpgrade, upgradeNewContract: upgradeNewContract},
			expectedEmitterChain:   vaa.GovernanceChain,
			expectedEmitterAddress: vaa.GovernanceEmitter,
			checkPayload: func(t *testing.T, payload []byte) {
				const contractUpgradePayloadLength = 67
				if len(payload) != contractUpgradePayloadLength {
					t.Fatalf("payload length %d, expected %d", len(payload), contractUpgradePayloadLength)
				}
				if !bytes.Equal(payload, expectedContractUpgradePayload) {
					t.Fatalf("payload %x, expected %x", payload, expectedContractUpgradePayload)
				}
			},
		},
		{
			name:                   "message",
			request:                vaaRequest{kind: vaaKindMessage},
			expectedEmitterChain:   vaa.ChainIDEthereum,
			expectedEmitterAddress: vaa.Address{31: 0x2a},
			checkPayload: func(t *testing.T, payload []byte) {
				if string(payload) != "canton localnet rehearsal message" {
					t.Fatalf("payload %q", payload)
				}
			},
		},
	}
	for _, testCase := range testCases {
		t.Run(testCase.name, func(t *testing.T) {
			emitterChain, emitterAddress, payload, err := buildPayload(testCase.request)
			if err != nil {
				t.Fatalf("unexpected error: %v", err)
			}
			if emitterChain != testCase.expectedEmitterChain {
				t.Fatalf("emitter chain %d, expected %d", emitterChain, testCase.expectedEmitterChain)
			}
			if emitterAddress != testCase.expectedEmitterAddress {
				t.Fatalf("emitter address %x, expected %x", emitterAddress, testCase.expectedEmitterAddress)
			}
			testCase.checkPayload(t, payload)
		})
	}
}

const (
	goldenSetMessageFeePayloadHex = "00000000000000000000000000000000000000000000000000000000436f726503004b" +
		"00000000000000000000000000000000000000000000000000000000000003e8"
	goldenTransferFeesPayloadHex = "00000000000000000000000000000000000000000000000000000000436f726504004b" +
		"0000000000000000000000000000000000000000000000000000000000000009" +
		"d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4"
	goldenContractUpgradePayloadHex = "00000000000000000000000000000000000000000000000000000000436f726501004b" +
		"c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1"
	// Same bytes as the payload of the devnet guardian-set-upgrade VAA in clients/eth/.vscode/launch.json.
	goldenGuardianSetUpgradeToSet1PayloadHex = "00000000000000000000000000000000000000000000000000000000436f7265020000" +
		"0000000102befa429d57cd18b7f8a4d91a2da9ab4af05d0fbe88d7d8b32a9105d228100e72dffe2fae0705d31c"
	goldenGuardianSetUpgradeToSet2PayloadHex = "00000000000000000000000000000000000000000000000000000000436f7265020000" +
		"0000000203" + "58076f561cc62a47087b567c86f986426dfcd000" + "bd6e9833490f8fa87c733a183cd076a6cbd29074" +
		"b853fcf0a5c78c1b56d15fce7a154e6ebe9ed7a2"
	goldenMessagePayloadHex = "63616e746f6e206c6f63616c6e65742072656865617273616c206d657373616765"
)

func TestBuildPayloadMatchesGoldenVectors(t *testing.T) {
	testCases := []struct {
		name               string
		request            vaaRequest
		expectedPayloadHex string
	}{
		{"set-message-fee 1000", vaaRequest{kind: vaaKindSetMessageFee, messageFee: 1000}, goldenSetMessageFeePayloadHex},
		{"transfer-fees 9", vaaRequest{kind: vaaKindTransferFees, transferFeesAmount: 9,
			transferFeesRecipient: vaa.Address(bytes.Repeat([]byte{0xd4}, 32))}, goldenTransferFeesPayloadHex},
		{"contract-upgrade", vaaRequest{kind: vaaKindContractUpgrade,
			upgradeNewContract: vaa.Address(bytes.Repeat([]byte{0xc1}, 32))}, goldenContractUpgradePayloadHex},
		{"guardian-set-upgrade to set 1 with keys 0,1", vaaRequest{kind: vaaKindGuardianSetUpgrade, newSetIndex: 1,
			newKeyIndices: []uint64{0, 1}}, goldenGuardianSetUpgradeToSet1PayloadHex},
		{"guardian-set-upgrade to set 2 with keys 2,3,4", vaaRequest{kind: vaaKindGuardianSetUpgrade, newSetIndex: 2,
			newKeyIndices: []uint64{2, 3, 4}}, goldenGuardianSetUpgradeToSet2PayloadHex},
		{"message", vaaRequest{kind: vaaKindMessage}, goldenMessagePayloadHex},
	}
	for _, testCase := range testCases {
		t.Run(testCase.name, func(t *testing.T) {
			_, _, payload, err := buildPayload(testCase.request)
			if err != nil {
				t.Fatalf("unexpected error: %v", err)
			}
			if hex.EncodeToString(payload) != testCase.expectedPayloadHex {
				t.Fatalf("payload %x, expected %s", payload, testCase.expectedPayloadHex)
			}
		})
	}
}

// Builds each governance payload from the byte layout, without sdk/vaa, so the golden vectors are not circular.
func TestGoldenVectorsMatchHandBuiltGovernanceLayout(t *testing.T) {
	coreModule := append(make([]byte, 28), []byte("Core")...)
	cantonChain := []byte{0x00, 0x4b}
	allChains := []byte{0x00, 0x00}
	uint256BigEndian := func(amount byte) []byte { return append(make([]byte, 31), amount) }
	concatenate := func(fields ...[]byte) []byte { return bytes.Join(fields, nil) }
	mustDecodeHex := func(hexText string) []byte {
		decoded, err := hex.DecodeString(hexText)
		if err != nil {
			t.Fatalf("decode %q: %v", hexText, err)
		}
		return decoded
	}
	testCases := []struct {
		name             string
		handBuiltPayload []byte
		goldenPayloadHex string
	}{
		{"set-message-fee: module, action 3, chain 75, fee 1000",
			concatenate(coreModule, []byte{3}, cantonChain, append(make([]byte, 30), 0x03, 0xe8)), goldenSetMessageFeePayloadHex},
		{"transfer-fees: module, action 4, chain 75, amount 9, recipient",
			concatenate(coreModule, []byte{4}, cantonChain, uint256BigEndian(9), bytes.Repeat([]byte{0xd4}, 32)), goldenTransferFeesPayloadHex},
		{"contract-upgrade: module, action 1, chain 75, new contract",
			concatenate(coreModule, []byte{1}, cantonChain, bytes.Repeat([]byte{0xc1}, 32)), goldenContractUpgradePayloadHex},
		{"guardian-set-upgrade: module, action 2, chain 0, index 1, two devnet guardian addresses",
			concatenate(coreModule, []byte{2}, allChains, []byte{0, 0, 0, 1}, []byte{2},
				mustDecodeHex("befa429d57cd18b7f8a4d91a2da9ab4af05d0fbe"), mustDecodeHex("88d7d8b32a9105d228100e72dffe2fae0705d31c")),
			goldenGuardianSetUpgradeToSet1PayloadHex},
	}
	for _, testCase := range testCases {
		t.Run(testCase.name, func(t *testing.T) {
			if hex.EncodeToString(testCase.handBuiltPayload) != testCase.goldenPayloadHex {
				t.Fatalf("hand-built payload %x, golden %s", testCase.handBuiltPayload, testCase.goldenPayloadHex)
			}
		})
	}
}

func TestNewGuardianPublicKeysHex(t *testing.T) {
	tests := []struct {
		name                 string
		request              vaaRequest
		expectedKeyIndices   []uint64
		expectNoPublicKeyHex bool
	}{
		{name: "guardian-set-upgrade lists each new key in set order",
			request:            vaaRequest{kind: vaaKindGuardianSetUpgrade, newSetIndex: 2, newKeyIndices: []uint64{2, 3, 4}},
			expectedKeyIndices: []uint64{2, 3, 4}},
		{name: "guardian-set-upgrade to one key",
			request:            vaaRequest{kind: vaaKindGuardianSetUpgrade, newSetIndex: 1, newKeyIndices: []uint64{1}},
			expectedKeyIndices: []uint64{1}},
		{name: "message has no new keys", request: vaaRequest{kind: vaaKindMessage}, expectNoPublicKeyHex: true},
		{name: "set-message-fee has no new keys", request: vaaRequest{kind: vaaKindSetMessageFee}, expectNoPublicKeyHex: true},
		{name: "message ignores new key indices",
			request:              vaaRequest{kind: vaaKindMessage, newKeyIndices: []uint64{1}},
			expectNoPublicKeyHex: true},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			publicKeysHex := newGuardianPublicKeysHex(tc.request)
			if tc.expectNoPublicKeyHex {
				if publicKeysHex != nil {
					t.Fatalf("got %v, expected nil", publicKeysHex)
				}
				return
			}
			if len(publicKeysHex) != len(tc.expectedKeyIndices) {
				t.Fatalf("got %d keys, expected %d", len(publicKeysHex), len(tc.expectedKeyIndices))
			}
			for position, keyIndex := range tc.expectedKeyIndices {
				publicKey, err := hex.DecodeString(publicKeysHex[position])
				if err != nil {
					t.Fatalf("key %d is not hex: %v", position, err)
				}
				if len(publicKey) != 65 {
					t.Fatalf("key %d has %d bytes, expected 65", position, len(publicKey))
				}
				if publicKey[0] != 0x04 {
					t.Fatalf("key %d starts with %#x, expected 0x04", position, publicKey[0])
				}
				expectedAddress := ethcrypto.PubkeyToAddress(devnet.InsecureDeterministicEcdsaKeyByIndex(keyIndex).PublicKey)
				derivedAddress := ethcommon.BytesToAddress(ethcrypto.Keccak256(publicKey[1:])[12:])
				if derivedAddress != expectedAddress {
					t.Fatalf("key %d derives %s, expected %s", position, derivedAddress.Hex(), expectedAddress.Hex())
				}
			}
		})
	}
}
