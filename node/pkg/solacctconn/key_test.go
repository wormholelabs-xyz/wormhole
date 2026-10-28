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
func writeKeygenFile(t *testing.T, key []byte, mode os.FileMode) string {
	t.Helper()
	raw, err := json.Marshal(key)
	require.NoError(t, err)
	path := filepath.Join(t.TempDir(), "payer.json")
	require.NoError(t, os.WriteFile(path, raw, mode))
	// WriteFile honours umask, so set the mode explicitly.
	require.NoError(t, os.Chmod(path, mode))
	return path
}

func TestLoadFeePayer(t *testing.T) {
	key, err := solana.NewRandomPrivateKey()
	require.NoError(t, err)
	require.Len(t, key, FeePayerKeyLen)

	tests := []struct {
		name    string
		setup   func(t *testing.T) string
		wantErr error
		wantMsg string
	}{
		{
			name:  "loads a private key",
			setup: func(t *testing.T) string { return writeKeygenFile(t, key, 0o600) },
		},
		{
			name:    "rejects a group readable file",
			setup:   func(t *testing.T) string { return writeKeygenFile(t, key, 0o640) },
			wantMsg: "group or world",
		},
		{
			// Kubernetes mounts each secret key as a symlink into a ..data directory.
			name: "loads a key through a symlink",
			setup: func(t *testing.T) string {
				link := filepath.Join(t.TempDir(), "payer-link.json")
				require.NoError(t, os.Symlink(writeKeygenFile(t, key, 0o600), link))
				return link
			},
		},
		{
			name: "rejects an oversized file",
			setup: func(t *testing.T) string {
				path := filepath.Join(t.TempDir(), "payer.json")
				f, err := os.OpenFile(path, os.O_CREATE|os.O_WRONLY, 0o600) // #nosec G304 -- test temp dir
				require.NoError(t, err)
				require.NoError(t, f.Truncate(common.MaxSafeInputSize+1))
				require.NoError(t, f.Close())
				return path
			},
			wantErr: common.ErrInputTooLarge,
		},
		{
			name: "rejects a fifo without blocking",
			setup: func(t *testing.T) string {
				path := filepath.Join(t.TempDir(), "payer.fifo")
				require.NoError(t, syscall.Mkfifo(path, 0o600))
				return path
			},
			wantMsg: "not a regular file",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got, err := LoadFeePayer(tt.setup(t))
			switch {
			case tt.wantErr != nil:
				require.ErrorIs(t, err, tt.wantErr)
			case tt.wantMsg != "":
				require.ErrorContains(t, err, tt.wantMsg)
			default:
				require.NoError(t, err)
				assert.Equal(t, key, got)
			}
		})
	}
}
