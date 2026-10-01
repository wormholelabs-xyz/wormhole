package solacctconn

import (
	"crypto/ed25519"
	"fmt"
	"os"

	"github.com/certusone/wormhole/node/pkg/common"
	"github.com/gagliardetto/solana-go"
)

const (
	// FeePayerKeyLen is the length of an ed25519 keypair as solana-keygen writes it: seed and
	// public key.
	FeePayerKeyLen = ed25519.PrivateKeySize

	groupOrWorldPermBits = 0o077
)

// LoadFeePayer reads a solana-keygen JSON keypair.
//
// SECURITY: after symlink resolution, group and world must not have read access to the file.
// The key is exactly FeePayerKeyLen bytes.
func LoadFeePayer(path string) (solana.PrivateKey, error) {
	// Opening a FIFO blocks, so the function checks the type before the open and after it.
	pre, err := os.Stat(path)
	if err != nil {
		return nil, fmt.Errorf("fee payer key: %w", err)
	}
	if !pre.Mode().IsRegular() {
		return nil, fmt.Errorf("fee payer key %s is not a regular file", path)
	}

	f, err := os.Open(path) // #nosec G304 -- operator-supplied key path
	if err != nil {
		return nil, fmt.Errorf("fee payer key: %w", err)
	}
	defer f.Close()

	// The function reads the mode from the open handle, so it checks the same file that it reads.
	info, err := f.Stat()
	if err != nil {
		return nil, fmt.Errorf("fee payer key: %w", err)
	}
	if !info.Mode().IsRegular() {
		return nil, fmt.Errorf("fee payer key %s is not a regular file", path)
	}
	if perm := info.Mode().Perm(); perm&groupOrWorldPermBits != 0 {
		return nil, fmt.Errorf("fee payer key %s is readable by group or world, mode %#o", path, perm)
	}

	content, err := common.SafeRead(f)
	if err != nil {
		return nil, fmt.Errorf("fee payer key: %w", err)
	}
	key, err := solana.PrivateKeyFromSolanaKeygenFileBytes(content)
	if err != nil {
		return nil, fmt.Errorf("fee payer key: %w", err)
	}
	if len(key) != FeePayerKeyLen {
		return nil, fmt.Errorf("fee payer key: want %d bytes, got %d", FeePayerKeyLen, len(key))
	}
	return key, nil
}
