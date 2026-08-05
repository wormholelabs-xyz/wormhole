// Package sign holds the custody-boundary implementations of ceremony.Signer
// and the Ed25519 key material helpers. Keys are X.509 SubjectPublicKeyInfo /
// PKCS#8 DER — the exact formats the Canton console recipe loads and the
// devnet guardian_key_tool.js produces.
package sign

import (
	"crypto/ed25519"
	"crypto/rand"
	"crypto/sha256"
	"crypto/x509"
	"encoding/hex"
	"fmt"
	"os"
	"os/exec"
	"strings"
)

// GenerateKeyFiles creates a fresh Ed25519 key pair and writes
// <prefix>.key (PKCS#8 DER private key, mode 0600) and <prefix>.pub
// (SubjectPublicKeyInfo DER public key). It returns the public key DER.
func GenerateKeyFiles(prefix string) ([]byte, error) {
	pub, priv, err := ed25519.GenerateKey(rand.Reader)
	if err != nil {
		return nil, fmt.Errorf("sign: generating key: %w", err)
	}
	privDER, err := x509.MarshalPKCS8PrivateKey(priv)
	if err != nil {
		return nil, fmt.Errorf("sign: encoding private key: %w", err)
	}
	pubDER, err := x509.MarshalPKIXPublicKey(pub)
	if err != nil {
		return nil, fmt.Errorf("sign: encoding public key: %w", err)
	}
	// O_EXCL: never clobber an existing private key — overwriting a guardian's
	// namespace key would be an unrecoverable custody loss.
	if err := writeNew(prefix+".key", privDER, 0o600); err != nil {
		return nil, fmt.Errorf("sign: writing private key: %w", err)
	}
	if err := writeNew(prefix+".pub", pubDER, 0o644); err != nil {
		return nil, fmt.Errorf("sign: writing public key: %w", err)
	}
	return pubDER, nil
}

func writeNew(path string, data []byte, perm os.FileMode) error {
	f, err := os.OpenFile(path, os.O_WRONLY|os.O_CREATE|os.O_EXCL, perm)
	if err != nil {
		return err
	}
	if _, err := f.Write(data); err != nil {
		f.Close()
		return err
	}
	return f.Close()
}

// PublicKeyFingerprint is a display fingerprint (sha256 hex of the DER key) for
// operator sanity-checking. It is NOT Canton's on-ledger fingerprint format;
// treat it only as a local "did I load the right key" aid.
func PublicKeyFingerprint(pubDER []byte) string {
	d := sha256.Sum256(pubDER)
	return hex.EncodeToString(d[:])
}

// ParsePublicKeyDER validates that der is an Ed25519 SubjectPublicKeyInfo key,
// so a wrong or corrupt file is rejected at load time rather than deep in a
// ceremony.
func ParsePublicKeyDER(der []byte) error {
	key, err := x509.ParsePKIXPublicKey(der)
	if err != nil {
		return fmt.Errorf("sign: not a valid public key: %w", err)
	}
	if _, ok := key.(ed25519.PublicKey); !ok {
		return fmt.Errorf("sign: public key is not Ed25519")
	}
	return nil
}

// KeySigner signs with an in-process Ed25519 private key loaded from a
// PKCS#8 DER file. Suitable for tests and local development; production
// guardians use CmdSigner against real custody.
type KeySigner struct {
	priv ed25519.PrivateKey
}

// NewKeySigner loads a PKCS#8 DER Ed25519 private key file.
func NewKeySigner(path string) (*KeySigner, error) {
	der, err := os.ReadFile(path)
	if err != nil {
		return nil, fmt.Errorf("sign: reading key file: %w", err)
	}
	key, err := x509.ParsePKCS8PrivateKey(der)
	if err != nil {
		return nil, fmt.Errorf("sign: parsing key file %s: %w", path, err)
	}
	edKey, ok := key.(ed25519.PrivateKey)
	if !ok {
		return nil, fmt.Errorf("sign: %s is not an Ed25519 key", path)
	}
	return &KeySigner{priv: edKey}, nil
}

// Sign implements ceremony.Signer: raw Ed25519 over the decoded hash bytes,
// returned as hex — the same contract the console recipe's external sign
// command follows.
func (k *KeySigner) Sign(hashHex string) (string, error) {
	hash, err := decodeHash(hashHex)
	if err != nil {
		return "", err
	}
	return hex.EncodeToString(ed25519.Sign(k.priv, hash)), nil
}

// sigLen is the byte length of a raw Ed25519 signature.
const sigLen = 64

// decodeHash validates and decodes a signing hash. It requires pure,
// non-empty hex — the length is backend-defined (Canton uses a 34-byte
// multihash-prefixed sha256), so it is not fixed here. Requiring hex is also
// the shell-injection guard for CmdSigner: a hex string carries no shell
// metacharacters, so it is safe to pass to `sh -c`.
func decodeHash(hashHex string) ([]byte, error) {
	if hashHex == "" {
		return nil, fmt.Errorf("sign: refusing empty hash")
	}
	hash, err := hex.DecodeString(hashHex)
	if err != nil {
		return nil, fmt.Errorf("sign: refusing non-hex hash %q: %w", hashHex, err)
	}
	return hash, nil
}

// PublicKeyDER returns the signer's public key as SubjectPublicKeyInfo DER.
func (k *KeySigner) PublicKeyDER() ([]byte, error) {
	pubDER, err := x509.MarshalPKIXPublicKey(k.priv.Public())
	if err != nil {
		return nil, fmt.Errorf("sign: encoding public key: %w", err)
	}
	return pubDER, nil
}

// CmdSigner is the pluggable custody boundary: it invokes an external command
// as `<command> <hashHex>` and expects a hex-encoded raw Ed25519 signature on
// stdout — the identical contract to the devnet recipe's GG_OWNER_SIGN_CMD
// and guardian_key_tool.js. In production the command wraps an HSM/KMS/offline
// signer; the ceremony process never sees private key material.
type CmdSigner struct {
	command string
}

// NewCmdSigner wraps a shell command string.
func NewCmdSigner(command string) (*CmdSigner, error) {
	if strings.TrimSpace(command) == "" {
		return nil, fmt.Errorf("sign: empty signer command")
	}
	return &CmdSigner{command: command}, nil
}

// Sign implements ceremony.Signer by shelling out to the custody command.
// The hash is validated as pure hex BEFORE it reaches the shell: it arrives
// from the shared ceremony state (a Git-distributed reports file), so an
// unvalidated value would be a shell-injection path into guardian custody.
func (c *CmdSigner) Sign(hashHex string) (string, error) {
	if _, err := decodeHash(hashHex); err != nil {
		return "", err
	}
	out, err := exec.Command("sh", "-c", c.command+" "+hashHex).Output()
	if err != nil {
		return "", fmt.Errorf("sign: custody command failed: %w", err)
	}
	sig := strings.TrimSpace(string(out))
	raw, err := hex.DecodeString(sig)
	if err != nil {
		return "", fmt.Errorf("sign: custody command returned non-hex output: %w", err)
	}
	if len(raw) != sigLen {
		return "", fmt.Errorf("sign: custody command returned a %d-byte signature, want %d", len(raw), sigLen)
	}
	return sig, nil
}
