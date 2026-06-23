package cantonclient

import (
	"testing"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

func TestOffsetTxIDRoundTrip(t *testing.T) {
	cases := []int64{0, 1, 42, 1 << 20, 1<<62 - 1}
	for _, offset := range cases {
		txID := OffsetToTxID(offset)
		require.Len(t, txID, offsetTxIDLen)
		got, err := TxIDToOffset(txID)
		require.NoError(t, err)
		assert.Equal(t, offset, got, "round trip for offset %d", offset)
	}
}

func TestOffsetToTxIDIsLeftPadded(t *testing.T) {
	txID := OffsetToTxID(1)
	require.Len(t, txID, 32)
	// All but the final byte must be zero; the last byte holds the value.
	for i := range 31 {
		assert.Zero(t, txID[i], "byte %d", i)
	}
	assert.Equal(t, byte(1), txID[31])
}

func TestTxIDToOffsetShortInput(t *testing.T) {
	// A shorter-than-32-byte input is right-aligned and still decodes.
	got, err := TxIDToOffset([]byte{0x01, 0x00})
	require.NoError(t, err)
	assert.Equal(t, int64(256), got)
}

func TestTxIDToOffsetTooLong(t *testing.T) {
	_, err := TxIDToOffset(make([]byte, 33))
	assert.Error(t, err)
}

func TestTxIDToOffsetOutOfRange(t *testing.T) {
	// A high byte set above the low 8 bytes is out of range.
	b := make([]byte, 32)
	b[0] = 0x01
	_, err := TxIDToOffset(b)
	assert.Error(t, err)
}
