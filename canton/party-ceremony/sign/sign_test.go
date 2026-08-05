package sign

import (
	"crypto/ed25519"
	"crypto/x509"
	"encoding/hex"
	"os"
	"path/filepath"
	"strings"
	"testing"
)

func TestKeySignerRoundTrip(t *testing.T) {
	prefix := filepath.Join(t.TempDir(), "owner")
	pubDER, err := GenerateKeyFiles(prefix)
	if err != nil {
		t.Fatalf("generating key files: %v", err)
	}
	signer, err := NewKeySigner(prefix + ".key")
	if err != nil {
		t.Fatalf("loading signer: %v", err)
	}

	hash := strings.Repeat("ab", 32)
	sigHex, err := signer.Sign(hash)
	if err != nil {
		t.Fatalf("signing: %v", err)
	}

	key, err := x509.ParsePKIXPublicKey(pubDER)
	if err != nil {
		t.Fatalf("parsing generated public key: %v", err)
	}
	pub, ok := key.(ed25519.PublicKey)
	if !ok {
		t.Fatalf("generated key is not Ed25519")
	}
	hashBytes, _ := hex.DecodeString(hash)
	sig, _ := hex.DecodeString(sigHex)
	if !ed25519.Verify(pub, hashBytes, sig) {
		t.Fatalf("signature does not verify against the generated public key")
	}

	roundTrip, err := signer.PublicKeyDER()
	if err != nil {
		t.Fatalf("deriving public key: %v", err)
	}
	if string(roundTrip) != string(pubDER) {
		t.Fatalf("signer-derived public key differs from generated file")
	}
}

func TestKeySignerRejectsBadInput(t *testing.T) {
	prefix := filepath.Join(t.TempDir(), "owner")
	if _, err := GenerateKeyFiles(prefix); err != nil {
		t.Fatalf("generating key files: %v", err)
	}
	signer, err := NewKeySigner(prefix + ".key")
	if err != nil {
		t.Fatalf("loading signer: %v", err)
	}
	if _, err := signer.Sign("not-hex"); err == nil {
		t.Fatalf("signed a non-hex hash")
	}
	if _, err := NewKeySigner(prefix + ".pub"); err == nil {
		t.Fatalf("loaded a public key file as a private key")
	}
}

func TestCmdSignerContract(t *testing.T) {
	hash := strings.Repeat("ab", 34)      // valid hex hash (Canton multihash length)
	sig64 := strings.Repeat("cd", sigLen) // valid 64-byte signature

	// Trailing '#' comments out the hash argument the signer appends, so the
	// command's output is exactly what we echo.
	signer, err := NewCmdSigner("echo " + sig64 + " #")
	if err != nil {
		t.Fatalf("building cmd signer: %v", err)
	}
	sig, err := signer.Sign(hash)
	if err != nil {
		t.Fatalf("cmd signer: %v", err)
	}
	if sig != sig64 {
		t.Fatalf("sig = %q, want %q", sig, sig64)
	}

	// A non-hex hash and a short signature are both rejected.
	if _, err := signer.Sign("nothex"); err == nil {
		t.Fatalf("accepted a non-hex hash")
	}
	short, _ := NewCmdSigner("echo deadbeef #")
	if _, err := short.Sign(hash); err == nil {
		t.Fatalf("accepted a 4-byte signature")
	}

	garbage, err := NewCmdSigner("echo not-hex-output #")
	if err != nil {
		t.Fatalf("building cmd signer: %v", err)
	}
	if _, err := garbage.Sign(hash); err == nil {
		t.Fatalf("accepted non-hex custody output")
	}

	failing, err := NewCmdSigner("exit 3")
	if err != nil {
		t.Fatalf("building cmd signer: %v", err)
	}
	if _, err := failing.Sign(hash); err == nil {
		t.Fatalf("accepted a failing custody command")
	}

	if _, err := NewCmdSigner("   "); err == nil {
		t.Fatalf("accepted an empty custody command")
	}
}

// TestCmdSignerRefusesShellInjection: the hash reaches the custody command
// through `sh -c`, and in production it arrives from a Git-shared reports
// file — so anything that is not pure hex must be refused BEFORE the shell
// sees it, and refusal must not execute the payload.
func TestCmdSignerRefusesShellInjection(t *testing.T) {
	marker := filepath.Join(t.TempDir(), "pwned")
	signer, err := NewCmdSigner("echo deadbeef #")
	if err != nil {
		t.Fatalf("building cmd signer: %v", err)
	}
	valid := strings.Repeat("ab", 34)
	payloads := []string{
		valid + "; touch " + marker,
		"$(touch " + marker + ")",
		"`touch " + marker + "`",
		valid + " && touch " + marker,
		"",
	}
	for _, payload := range payloads {
		if _, err := signer.Sign(payload); err == nil {
			t.Errorf("accepted malicious hash %q", payload)
		}
	}
	if _, err := os.Stat(marker); err == nil {
		t.Fatalf("injection payload executed: marker file exists")
	}
}
