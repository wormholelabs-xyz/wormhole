package cantonclient

import (
	"encoding/hex"
	"strings"
	"testing"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

// Update id reported by a LocalNet participant.
const sampleUpdateIDHex = "1220ac5126912ade361264f7d40c4711a72dc9b3d12e9ec044a07d5df586b2fe397b"

func TestParseUpdateIDRoundTrip(t *testing.T) {
	updateID, err := ParseUpdateID(sampleUpdateIDHex)
	require.NoError(t, err)
	assert.Equal(t, sampleUpdateIDHex, updateID.String())

	txID := updateID.TxID()
	require.Len(t, txID, UpdateIDTxIDLen)
	wantTxID, err := hex.DecodeString(sampleUpdateIDHex)
	require.NoError(t, err)
	assert.Equal(t, wantTxID, txID, "TxID is the hex-decoded update id")

	decoded, err := UpdateIDFromTxID(txID)
	require.NoError(t, err)
	assert.Equal(t, updateID, decoded)
}

func TestUpdateIDTxIDIsACopy(t *testing.T) {
	updateID, err := ParseUpdateID(sampleUpdateIDHex)
	require.NoError(t, err)
	txID := updateID.TxID()
	txID[UpdateIDTxIDLen-1] ^= 0xff
	assert.Equal(t, sampleUpdateIDHex, updateID.String())
}

func TestParseUpdateIDRejectsMalformed(t *testing.T) {
	cases := []struct {
		name        string
		updateIDHex string
	}{
		{name: "empty", updateIDHex: ""},
		{name: "one byte short", updateIDHex: sampleUpdateIDHex[:len(sampleUpdateIDHex)-2]},
		{name: "one byte long", updateIDHex: sampleUpdateIDHex + "00"},
		{name: "odd length", updateIDHex: sampleUpdateIDHex[:len(sampleUpdateIDHex)-1]},
		{name: "uppercase hex", updateIDHex: strings.ToUpper(sampleUpdateIDHex)},
		{name: "0x prefix", updateIDHex: "0x" + sampleUpdateIDHex[2:]},
		{name: "non-hex character", updateIDHex: sampleUpdateIDHex[:len(sampleUpdateIDHex)-1] + "g"},
		{name: "wrong hash function code", updateIDHex: "1320" + sampleUpdateIDHex[4:]},
		{name: "wrong digest length code", updateIDHex: "1221" + sampleUpdateIDHex[4:]},
		{name: "decimal offset", updateIDHex: "590"},
		{name: "non-hash ledger string", updateIDHex: "update-deposit-preapproval"},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			_, err := ParseUpdateID(tc.updateIDHex)
			assert.Error(t, err)
		})
	}
}

func TestUpdateIDFromTxIDRejectsMalformed(t *testing.T) {
	sampleTxID, err := hex.DecodeString(sampleUpdateIDHex)
	require.NoError(t, err)
	// Offset 590 in the 32-byte left-padded encoding of earlier guardian versions.
	legacyOffsetTxID := make([]byte, 32)
	legacyOffsetTxID[30] = 0x02
	legacyOffsetTxID[31] = 0x4e
	wrongPrefixTxID := append([]byte{0x13, 0x20}, sampleTxID[2:]...)

	cases := []struct {
		name string
		txID []byte
	}{
		{name: "nil", txID: nil},
		{name: "legacy 32-byte offset", txID: legacyOffsetTxID},
		{name: "one byte short", txID: sampleTxID[:UpdateIDTxIDLen-1]},
		{name: "one byte long", txID: append(append([]byte{}, sampleTxID...), 0x00)},
		{name: "wrong multihash prefix", txID: wrongPrefixTxID},
		{name: "all zero", txID: make([]byte, UpdateIDTxIDLen)},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			_, err := UpdateIDFromTxID(tc.txID)
			assert.Error(t, err)
		})
	}
}

func TestUpdateIDZeroValueIsNotParsed(t *testing.T) {
	updateID, err := ParseUpdateID(sampleUpdateIDHex)
	require.NoError(t, err)
	assert.False(t, updateID.IsZero())
	assert.True(t, UpdateID{}.IsZero())
}
