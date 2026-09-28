package solacctconn

import (
	"fmt"
	"os"

	"github.com/certusone/wormhole/node/pkg/common"
	"github.com/gagliardetto/solana-go"
)

const (
	// FeePayerKeyLen is the length of an ed25519 keypair as solana-keygen writes it.
	FeePayerKeyLen = 64

	groupOrWorldPermBits = 0o077
)

// LoadFeePayer reads a solana-keygen JSON keypair.
//
// SECURITY: the file, after symlink resolution, must not be readable by group or world, and
// the key is exactly FeePayerKeyLen bytes.
func LoadFeePayer(path string) (solana.PrivateKey, error) {
	// Opening a FIFO blocks, so the type is checked before the open as well as after.
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

	// Mode is read from the open handle so the checked file is the one read.
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
