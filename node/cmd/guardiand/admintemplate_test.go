package guardiand

import (
	"strings"
	"testing"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	"github.com/wormhole-foundation/wormhole/sdk/vaa"
)

func TestParseAddress(t *testing.T) {
	t.Run("hex with 0x prefix", func(t *testing.T) {
		got, err := parseAddress("0x000000000000000000000000000000000000000000000000000000000000beef")
		require.NoError(t, err)
		assert.Len(t, got, 64) // 32 bytes hex-encoded
		assert.Equal(t, "000000000000000000000000000000000000000000000000000000000000beef", got)
	})

	t.Run("hex with 0X prefix", func(t *testing.T) {
		got, err := parseAddress("0X" + strings.Repeat("0", 62) + "ab")
		require.NoError(t, err)
		assert.Equal(t, strings.Repeat("0", 62)+"ab", got)
	})

	t.Run("short hex left-padded to 32 bytes", func(t *testing.T) {
		got, err := parseAddress("0xbeef")
		require.NoError(t, err)
		assert.Equal(t, strings.Repeat("0", 60)+"beef", got)
	})

	t.Run("base58 solana address", func(t *testing.T) {
		// 32-byte base58-encoded address (a valid Solana pubkey form)
		_, err := parseAddress("11111111111111111111111111111111")
		require.NoError(t, err)
	})

	t.Run("invalid hex", func(t *testing.T) {
		_, err := parseAddress("not-hex-and-not-base58!!")
		require.Error(t, err)
	})

	t.Run("hex too long", func(t *testing.T) {
		_, err := parseAddress("0x" + strings.Repeat("ab", 33)) // 33 bytes > 32
		require.Error(t, err)
		assert.Contains(t, err.Error(), "longer than 32 bytes")
	})
}

func TestParseChainID(t *testing.T) {
	t.Run("by name", func(t *testing.T) {
		got, err := parseChainID("ethereum")
		require.NoError(t, err)
		assert.Equal(t, vaa.ChainIDEthereum, got)
	})

	t.Run("by uint16 numeric", func(t *testing.T) {
		got, err := parseChainID("42")
		require.NoError(t, err)
		assert.Equal(t, vaa.ChainID(42), got)
	})

	t.Run("invalid string", func(t *testing.T) {
		_, err := parseChainID("nope-not-a-chain")
		require.Error(t, err)
	})

	t.Run("numeric overflow", func(t *testing.T) {
		_, err := parseChainID("65536")
		require.Error(t, err)
	})
}

func TestIsValidUint256(t *testing.T) {
	t.Run("zero is valid", func(t *testing.T) {
		ok, err := isValidUint256("0")
		require.NoError(t, err)
		assert.True(t, ok)
	})

	t.Run("max uint256 is valid", func(t *testing.T) {
		// 2^256 - 1
		ok, err := isValidUint256("115792089237316195423570985008687907853269984665640564039457584007913129639935")
		require.NoError(t, err)
		assert.True(t, ok)
	})

	t.Run("over max uint256 is invalid", func(t *testing.T) {
		_, err := isValidUint256("115792089237316195423570985008687907853269984665640564039457584007913129639936")
		require.Error(t, err)
	})

	t.Run("typical mid-range value", func(t *testing.T) {
		ok, err := isValidUint256("1000000000000000000")
		require.NoError(t, err)
		assert.True(t, ok)
	})
}

func TestRandSeqNonce(t *testing.T) {
	seq1, nonce1 := randSeqNonce()
	seq2, nonce2 := randSeqNonce()
	assert.NotEqual(t, seq1, seq2)
	assert.NotEqual(t, nonce1, nonce2)
}
