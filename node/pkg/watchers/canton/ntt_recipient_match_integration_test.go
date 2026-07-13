//go:build integration

// Integration test closing the NTT recipient-binding "known test gap" noted in
// canton/README.md and Wormhole.Ntt.Manager: no automated test exercised the
// MATCHING-recipient path through a real `Receive` call, because the existing
// receive fixture is a single, statically pre-signed VAA with a fixed
// recipientAddress, and a live Party's fingerprint is freshly and
// unpredictably allocated every run -- so no allocated Party can ever be made
// to match a value baked into an already-signed VAA ahead of time.
//
// This test closes it with the live two-step harness the gap note calls for:
//
//  1. Run Test.TestNtt:integrationNttReceiveMatchSetup against a live sandbox.
//     It allocates the recipient (and everything else Receive needs) and
//     returns recipientAddressFor(recipient) -- computed ON-LEDGER from the
//     ACTUAL allocated Party, not guessed.
//
//  2. Sign a FRESH NTT transfer VAA against that hash here, in Go, using the
//     same well-known devnet guardian test key
//     node/pkg/cantonclient/vectorgen_test.go signs every other fixture with
//     -- exactly the off-chain step a real sender takes.
//
//  3. Run Test.TestNtt:integrationNttReceiveMatch against the SAME sandbox
//     (ledger state persists across both dpm-script calls), relaying the
//     fresh VAA. Its internal assertions (mint lands on the matching
//     recipient; a replay of the same VAA is rejected) make `dpm script`
//     exit non-zero if either fails.
//
//     go test -tags integration -run TestCantonNttReceiveMatchIntegration ./pkg/watchers/canton -v
//
// Requires `dpm` (PATH or ~/.dpm/bin) + a JDK; skipped otherwise. Slow (boots a
// JVM sandbox), so it is excluded from the default build. findDpm/runCmd are
// shared with watcher_integration_test.go.
package canton

import (
	"context"
	"encoding/binary"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"net"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"syscall"
	"testing"
	"time"

	"github.com/ethereum/go-ethereum/crypto"
	"github.com/stretchr/testify/require"
)

// b32Match returns a 32-byte slice whose last byte is `last` (matching the
// 0x00..XX peer/manager addresses baked into every NTT Daml test fixture).
func b32Match(last byte) []byte {
	b := make([]byte, 32)
	b[31] = last
	return b
}

// lenPrefixedMatch prepends a uint16 big-endian length to b, matching the NTT
// wire codec's length-prefixed fields (Wormhole.Ntt.Payload).
func lenPrefixedMatch(b []byte) []byte {
	out := make([]byte, 2)
	binary.BigEndian.PutUint16(out, uint16(len(b))) //nolint:gosec // fixture fields are small
	return append(out, b...)
}

// signNttTransferVAA builds and signs an NTT transfer VAA -- source chain 2,
// peer transceiver 0x..cc (the VAA emitter), peer manager 0x..bb, our manager
// 0x..aa, a transfer of 1_000_000 @ 8 decimals of token 0x..dd -- with a
// caller-supplied recipientAddress, using the well-known Wormhole devnet
// guardian key (the same one node/pkg/cantonclient/vectorgen_test.go signs
// every other fixture in this package with). This is the reusable form of
// vectorgen_test.go's TestGenerateNttVector, parameterized by
// recipientAddress instead of the hardcoded 0x..ee, so it can sign a fresh VAA
// against a Party's ACTUAL, just-allocated hash rather than a value baked in
// ahead of time.
func signNttTransferVAA(recipientAddress [32]byte, sequence uint64) ([]byte, error) {
	priv, err := crypto.HexToECDSA("cfb12303a19cde580bb4dd771639b0d26bc68353645571a8cff516ab2ee113a0")
	if err != nil {
		return nil, err
	}

	// NativeTokenTransfer.
	ntt := []byte{0x99, 0x4e, 0x54, 0x54} // prefix
	ntt = append(ntt, 0x08)               // decimals 8
	amount := make([]byte, 8)
	binary.BigEndian.PutUint64(amount, 1_000_000)
	ntt = append(ntt, amount...)
	ntt = append(ntt, b32Match(0xdd)...)      // sourceToken
	ntt = append(ntt, recipientAddress[:]...) // recipientAddress (caller-supplied)
	ntt = append(ntt, 0x00, 0x48)             // recipientChain 72

	// NttManagerMessage: id = sequence, left-padded to 32 bytes big-endian;
	// sender = peer manager 0x..bb; payload = ntt. Receive doesn't check id
	// against anything, but it must vary across signed VAAs to avoid
	// accidentally reproducing the static nttTransferVAA fixture's digest.
	id := make([]byte, 32)
	binary.BigEndian.PutUint64(id[24:], sequence)
	mm := append([]byte{}, id...)
	mm = append(mm, b32Match(0xbb)...)
	mm = append(mm, lenPrefixedMatch(ntt)...)

	// WormholeTransceiverMessage: source = peer manager 0x..bb, recipient = our
	// manager 0x..aa, managerPayload = mm, transceiverPayload = empty.
	wm := []byte{0x99, 0x45, 0xff, 0x10}
	wm = append(wm, b32Match(0xbb)...)
	wm = append(wm, b32Match(0xaa)...)
	wm = append(wm, lenPrefixedMatch(mm)...)
	wm = append(wm, lenPrefixedMatch([]byte{})...)

	// VAA body: emitter chain 2, emitter = peer transceiver 0x..cc.
	body := make([]byte, 0)
	ts := make([]byte, 4)
	binary.BigEndian.PutUint32(ts, 1700000000)
	body = append(body, ts...)      // timestamp
	body = append(body, 0, 0, 0, 0) // nonce 0
	body = append(body, 0x00, 0x02) // emitterChain 2
	body = append(body, b32Match(0xcc)...)
	seqBytes := make([]byte, 8)
	binary.BigEndian.PutUint64(seqBytes, sequence)
	body = append(body, seqBytes...) // sequence
	body = append(body, 0x00)        // consistencyLevel 0
	body = append(body, wm...)       // payload

	digest := crypto.Keccak256(crypto.Keccak256(body))
	sig, err := crypto.Sign(digest, priv)
	if err != nil {
		return nil, err
	}

	vaa := []byte{0x01}           // version
	vaa = append(vaa, 0, 0, 0, 0) // guardianSetIndex 0
	vaa = append(vaa, 0x01)       // sig count
	vaa = append(vaa, 0x00)       // guardian index 0
	vaa = append(vaa, sig...)
	vaa = append(vaa, body...)
	return vaa, nil
}

// nttReceiveMatchSetup mirrors Test.TestNtt:NttReceiveMatchSetup's JSON shape.
type nttReceiveMatchSetup struct {
	MgrId            string `json:"mgrId"`
	Operator         string `json:"operator"`
	Recipient        string `json:"recipient"`
	CoreStateCid     string `json:"coreStateCid"`
	ReplayNodeCid    string `json:"replayNodeCid"`
	RecipientAddress string `json:"recipientAddress"`
}

func TestCantonNttReceiveMatchIntegration(t *testing.T) {
	dpm := findDpm(t)

	cantonDir, err := filepath.Abs("../../../../canton")
	require.NoError(t, err)
	dar := filepath.Join(cantonDir, "test", ".daml", "dist", "wormhole-core-test-0.1.0.dar")

	port := os.Getenv("CANTON_SANDBOX_PORT")
	if port == "" {
		port = "6865"
	}
	addr := "localhost:" + port

	runCmd(t, cantonDir, dpm, "build", "--all")

	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	sandboxDir := t.TempDir()
	logPath := filepath.Join(sandboxDir, "sandbox.log")
	logFile, err := os.Create(logPath)
	require.NoError(t, err)
	defer logFile.Close()

	sandbox := exec.CommandContext(ctx, dpm, "sandbox", "--no-tty")
	sandbox.Dir = sandboxDir
	sandbox.Stdout = logFile
	sandbox.Stderr = logFile
	sandbox.SysProcAttr = &syscall.SysProcAttr{Setpgid: true}
	require.NoError(t, sandbox.Start())
	t.Cleanup(func() {
		if sandbox.Process != nil {
			_ = syscall.Kill(-sandbox.Process.Pid, syscall.SIGKILL)
		}
	})

	require.Eventually(t, func() bool {
		b, _ := os.ReadFile(logPath)
		return strings.Contains(string(b), "Canton sandbox is ready")
	}, 180*time.Second, 2*time.Second, "sandbox never became ready")
	conn, err := net.DialTimeout("tcp", addr, 5*time.Second)
	require.NoError(t, err)
	_ = conn.Close()

	// Step 1: allocate the recipient (and everything else Receive needs) and
	// get its ACTUAL, on-ledger-computed recipientAddressFor hash.
	setupFile := filepath.Join(sandboxDir, "setup.json")
	out, err := exec.Command(dpm, "script", "--dar", dar, "--upload-dar", "yes",
		"--script-name", "Test.TestNtt:integrationNttReceiveMatchSetup",
		"--ledger-host", "localhost", "--ledger-port", port,
		"--output-file", setupFile).CombinedOutput()
	require.NoErrorf(t, err, "setup dpm script failed: %s", out)

	rawSetup, err := os.ReadFile(setupFile)
	require.NoError(t, err)
	var setup nttReceiveMatchSetup
	require.NoError(t, json.Unmarshal(rawSetup, &setup))
	require.NotEmpty(t, setup.Recipient)
	require.Contains(t, setup.Recipient, "::", "recipient should be a full party id")
	t.Logf("step 1: recipient=%s recipientAddress=%s", setup.Recipient, setup.RecipientAddress)

	recipientAddrBytes, err := hex.DecodeString(strings.TrimPrefix(setup.RecipientAddress, "0x"))
	require.NoError(t, err)
	require.Len(t, recipientAddrBytes, 32, "recipientAddressFor must be 32 bytes")
	var recipientAddr32 [32]byte
	copy(recipientAddr32[:], recipientAddrBytes)

	// Step 2: sign a FRESH VAA, off-chain, against that ACTUAL hash -- exactly
	// what a real sender does.
	vaaBytes, err := signNttTransferVAA(recipientAddr32, 1)
	require.NoError(t, err)
	t.Logf("step 2: signed a fresh VAA (%d bytes) for recipientAddress=%s", len(vaaBytes), setup.RecipientAddress)

	// Step 3: relay it against the SAME sandbox/manager/recipient from step 1.
	// All internal assertions (mint lands on the matching recipient; replay is
	// rejected) live inside the script; a non-zero exit means one failed.
	inputPayload := fmt.Sprintf(
		`{"mgrId":%q,"operator":%q,"recipient":%q,"coreStateCid":%q,"replayNodeCid":%q,"vaaBytes":%q}`,
		setup.MgrId, setup.Operator, setup.Recipient, setup.CoreStateCid, setup.ReplayNodeCid, hex.EncodeToString(vaaBytes),
	)
	inputFile := filepath.Join(sandboxDir, "input.json")
	require.NoError(t, os.WriteFile(inputFile, []byte(inputPayload), 0o600))

	out, err = exec.Command(dpm, "script", "--dar", dar,
		"--script-name", "Test.TestNtt:integrationNttReceiveMatch",
		"--ledger-host", "localhost", "--ledger-port", port,
		"--input-file", inputFile).CombinedOutput()
	require.NoErrorf(t, err, "receive-match dpm script failed: %s", out)
	t.Logf("step 3: matching-recipient Receive + replay rejection both verified live: %s", out)
}
