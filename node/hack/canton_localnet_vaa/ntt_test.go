package main

import (
	"encoding/hex"
	"strings"
	"testing"

	"github.com/stretchr/testify/require"
)

func mustHex32(t *testing.T, hexText string) [32]byte {
	t.Helper()
	address, err := parseBytes32Hex(hexText)
	require.NoError(t, err)
	return address
}

// Vectors pinned by canton/ntt Test.TestNttVectors and Test.TestNtt in the NTT repository.
func TestNttVectors(t *testing.T) {
	transfer := nttTransfer{
		SourceManager:    mustHex32(t, "042942fafabe0000000000000000000000000000000000000000000000000000"),
		RecipientManager: mustHex32(t, "042942fababe0000000000000000000000000000000000000000000000000000"),
		MessageID:        mustHex32(t, "128434bafe23430000000000000000000000000000000000ce00aa0000000000"),
		Sender:           mustHex32(t, "4667921341234300000000000000000000000000000000000000000000000000"),
		TrimmedDecimals:  7,
		TrimmedAmount:    1234567,
		SourceToken:      mustHex32(t, "beefface00000000000000000000000000000000000000000000000000000000"),
		RecipientAddress: mustHex32(t, "feebcafe00000000000000000000000000000000000000000000000000000000"),
		RecipientChain:   2,
	}
	const transceiverMessage1 = "9945ff10042942fafabe0000000000000000000000000000000000000000000000000000042942fababe00000000000000000000000000000000000000000000000000000091128434bafe23430000000000000000000000000000000000ce00aa00000000004667921341234300000000000000000000000000000000000000000000000000004f994e545407000000000012d687beefface00000000000000000000000000000000000000000000000000000000feebcafe0000000000000000000000000000000000000000000000000000000000020000"

	tests := []struct {
		name     string
		actual   string
		expected string
	}{
		{"transceiver message 1", hex.EncodeToString(encodeWormholeTransceiverMessage(transfer)), transceiverMessage1},
		{"recipient address", hex.EncodeToString(nttRecipientAddressForPartyText("vector-recipient::1220deadbeef")), "dbd2d1b09ea5715be3e45d0e38e5c5c0f6fcd0f181d45a9620dfb20f8ae3a4d9"},
		{"instrument id", nttInstrumentIDForAdminText("vector-admin::1220deadbeef", 0), "wormhole-ntt:a18ac0519eb929c6247f1924492c28a752101aa9c542eda3bd8fee8a296f7133"},
	}
	for _, testCase := range tests {
		t.Run(testCase.name, func(t *testing.T) {
			require.Equal(t, testCase.expected, testCase.actual)
		})
	}
}

func TestParseBytes32HexRejectsMalformedInput(t *testing.T) {
	tests := []struct {
		name    string
		hexText string
	}{
		{"empty", ""},
		{"31 bytes", "00000000000000000000000000000000000000000000000000000000000000"},
		{"33 bytes", "000000000000000000000000000000000000000000000000000000000000000000"},
		{"uppercase", "ABCDEF0000000000000000000000000000000000000000000000000000000000"},
		{"not hex", "zz00000000000000000000000000000000000000000000000000000000000000"},
	}
	for _, testCase := range tests {
		t.Run(testCase.name, func(t *testing.T) {
			_, err := parseBytes32Hex(testCase.hexText)
			require.Error(t, err)
		})
	}
}

func validNttTransferFlags() nttTransferFlags {
	address := strings.Repeat("22", 32)
	emitterChain, recipientChain, trimmedDecimals := uint(2), uint(75), uint(8)
	messageID, trimmedAmount := uint64(7), uint64(250000000)
	sender, recipientParty := strings.Repeat("11", 32), "recipient::1220deadbeef"
	emitterAddress, sourceManager, recipientManager, sourceToken := address, address, address, address
	return nttTransferFlags{
		emitterChain: &emitterChain, emitterAddress: &emitterAddress, sourceManager: &sourceManager,
		recipientManager: &recipientManager, sender: &sender, messageID: &messageID,
		trimmedDecimals: &trimmedDecimals, trimmedAmount: &trimmedAmount, sourceToken: &sourceToken,
		recipientParty: &recipientParty, recipientChain: &recipientChain,
	}
}

func TestNttTransferFromFlags(t *testing.T) {
	emitterChain, _, transfer, err := nttTransferFromFlags(validNttTransferFlags())
	require.NoError(t, err)
	require.Equal(t, uint16(2), uint16(emitterChain))
	require.Equal(t, uint16(75), transfer.RecipientChain)
	require.Equal(t, byte(7), transfer.MessageID[31])
	require.Equal(t, nttRecipientAddressForPartyText("recipient::1220deadbeef"), transfer.RecipientAddress[:])

	tests := []struct {
		name   string
		mutate func(flags nttTransferFlags)
	}{
		{"zero emitter chain", func(flags nttTransferFlags) { *flags.emitterChain = 0 }},
		{"emitter chain above uint16", func(flags nttTransferFlags) { *flags.emitterChain = 65536 }},
		{"zero recipient chain", func(flags nttTransferFlags) { *flags.recipientChain = 0 }},
		{"trimmed decimals above 8", func(flags nttTransferFlags) { *flags.trimmedDecimals = 9 }},
		{"zero trimmed amount", func(flags nttTransferFlags) { *flags.trimmedAmount = 0 }},
		{"missing recipient party", func(flags nttTransferFlags) { *flags.recipientParty = "" }},
		{"short emitter address", func(flags nttTransferFlags) { *flags.emitterAddress = "22" }},
		{"uppercase source manager", func(flags nttTransferFlags) { *flags.sourceManager = strings.Repeat("AA", 32) }},
	}
	for _, testCase := range tests {
		t.Run(testCase.name, func(t *testing.T) {
			flags := validNttTransferFlags()
			testCase.mutate(flags)
			_, _, _, err := nttTransferFromFlags(flags)
			require.Error(t, err)
		})
	}
}
