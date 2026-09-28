package solacctconn

import (
	"encoding/json"
	"os"
	"path/filepath"
	"syscall"
	"testing"

	"github.com/certusone/wormhole/node/pkg/common"
	"github.com/gagliardetto/solana-go"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

// writeKeygenFile writes bytes in the solana-keygen JSON array format.
func writeKeygenFile(t *testing.T, name string, key []byte, mode os.FileMode) string {
	t.Helper()
	raw, err := json.Marshal(key)
	require.NoError(t, err)
	path := filepath.Join(t.TempDir(), name)
	require.NoError(t, os.WriteFile(path, raw, mode))
	// WriteFile honours umask, so set the mode explicitly.
	require.NoError(t, os.Chmod(path, mode))
	return path
}

func TestLoadFeePayer(t *testing.T) {
	key, err := solana.NewRandomPrivateKey()
	require.NoError(t, err)
	require.Len(t, key, FeePayerKeyLen)

	t.Run("loads a private key", func(t *testing.T) {
		path := writeKeygenFile(t, "payer.json", key, 0o600)
		got, err := LoadFeePayer(path)
		require.NoError(t, err)
		assert.Equal(t, key, got)
	})

	t.Run("rejects a group readable file", func(t *testing.T) {
		path := writeKeygenFile(t, "payer.json", key, 0o640)
		_, err := LoadFeePayer(path)
		require.Error(t, err)
		assert.Contains(t, err.Error(), "group or world")
	})

	t.Run("rejects a world readable file", func(t *testing.T) {
		path := writeKeygenFile(t, "payer.json", key, 0o604)
		_, err := LoadFeePayer(path)
		require.Error(t, err)
	})

	t.Run("rejects the wrong key length", func(t *testing.T) {
		path := writeKeygenFile(t, "payer.json", key[:32], 0o600)
		_, err := LoadFeePayer(path)
		require.Error(t, err)
	})

	t.Run("rejects a key whose public half does not match", func(t *testing.T) {
		tampered := make([]byte, FeePayerKeyLen)
		copy(tampered, key)
		tampered[FeePayerKeyLen-1] ^= 0xff
		path := writeKeygenFile(t, "payer.json", tampered, 0o600)
		_, err := LoadFeePayer(path)
		require.Error(t, err)
	})

	// Kubernetes mounts each secret key as a symlink into a ..data directory.
	t.Run("loads a key through a symlink", func(t *testing.T) {
		target := writeKeygenFile(t, "payer.json", key, 0o600)
		link := filepath.Join(t.TempDir(), "payer-link.json")
		require.NoError(t, os.Symlink(target, link))
		got, err := LoadFeePayer(link)
		require.NoError(t, err)
		assert.Equal(t, key, got)
	})

	t.Run("rejects a symlink to a group readable file", func(t *testing.T) {
		target := writeKeygenFile(t, "payer.json", key, 0o640)
		link := filepath.Join(t.TempDir(), "payer-link.json")
		require.NoError(t, os.Symlink(target, link))
		_, err := LoadFeePayer(link)
		require.Error(t, err)
		assert.Contains(t, err.Error(), "group or world")
	})

	t.Run("rejects a symlink to a fifo without blocking", func(t *testing.T) {
		target := filepath.Join(t.TempDir(), "payer.fifo")
		require.NoError(t, syscall.Mkfifo(target, 0o600))
		link := filepath.Join(t.TempDir(), "payer-link.json")
		require.NoError(t, os.Symlink(target, link))
		_, err := LoadFeePayer(link)
		require.Error(t, err)
		assert.Contains(t, err.Error(), "not a regular file")
	})

	t.Run("rejects a missing file", func(t *testing.T) {
		_, err := LoadFeePayer(filepath.Join(t.TempDir(), "absent.json"))
		require.Error(t, err)
	})

	t.Run("rejects an oversized file", func(t *testing.T) {
		path := filepath.Join(t.TempDir(), "payer.json")
		f, err := os.OpenFile(path, os.O_CREATE|os.O_WRONLY, 0o600) // #nosec G304 -- test temp dir
		require.NoError(t, err)
		require.NoError(t, f.Truncate(common.MaxSafeInputSize+1))
		require.NoError(t, f.Close())
		_, err = LoadFeePayer(path)
		require.ErrorIs(t, err, common.ErrInputTooLarge)
	})

	t.Run("rejects a directory", func(t *testing.T) {
		_, err := LoadFeePayer(t.TempDir())
		require.Error(t, err)
	})

	t.Run("rejects a fifo without blocking", func(t *testing.T) {
		path := filepath.Join(t.TempDir(), "payer.fifo")
		require.NoError(t, syscall.Mkfifo(path, 0o600))
		_, err := LoadFeePayer(path)
		require.Error(t, err)
		assert.Contains(t, err.Error(), "not a regular file")
	})
}
