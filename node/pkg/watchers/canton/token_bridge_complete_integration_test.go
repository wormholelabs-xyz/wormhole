//go:build integration

// Integration test closing the token-bridge recipient/tokenAddress-binding
// "known test gap" noted in TestTokenBridge.daml: no automated test exercises
// the MATCHING path through a real CompleteTransfer call, because the static
// VAA fixtures used elsewhere in that suite are pre-signed against fixed,
// arbitrary tokenAddress/recipient values, and a live Party's fingerprint
// (and a live InstrumentId's admin party) are freshly and unpredictably
// allocated every run -- so nothing allocated in a Daml Script test can ever
// be made to match a value baked into an already-signed VAA ahead of time.
//
// This closes it with the live two-step harness the gap note calls for,
// mirroring ntt_recipient_match_integration_test.go:
//
//  1. Run Wormhole.Example.Test.TestTokenBridge:integrationCompleteTransferSetup
//     against a live sandbox. It allocates the recipient/admin/bridge/custody
//     holding CompleteTransfer needs and returns tokenAddressFor(instrumentId)
//     and tokenBridgeRecipientAddressFor(recipient) -- computed ON-LEDGER from
//     the ACTUAL allocated parties, not guessed.
//
//  2. Sign a FRESH Token Bridge Transfer VAA against those hashes here, in
//     Go, using the same well-known devnet guardian test key
//     node/pkg/cantonclient/vectorgen_test.go signs every other fixture with
//     -- exactly the off-chain step a real sender takes.
//
//  3. Run Wormhole.Example.Test.TestTokenBridge:integrationCompleteTransfer
//     against the SAME sandbox (ledger state persists across both dpm-script
//     calls), relaying the fresh VAA. Its internal assertions (the unlock
//     lands on the matching recipient; a replay of the same VAA is rejected)
//     make `dpm script` exit non-zero if either fails.
//
//     go test -tags integration -run TestCantonTokenBridgeCompleteIntegration ./pkg/watchers/canton -v
//
// Requires `dpm` (PATH or ~/.dpm/bin) + a JDK; skipped otherwise. Slow (boots
// a JVM sandbox), so it is excluded from the default build. findDpm/runCmd
// are shared with watcher_integration_test.go.
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

// signTokenBridgeTransferVAA builds and signs a whitepaper-0003 Token Bridge
// Transfer VAA -- source chain 2, peer token-bridge emitter
// 0x00..00beef (matching the peerChainId/peerEmitterAddress test constants in
// TestTokenBridge.daml), amount 50.0 at 8dp (5_000_000_000, matching the
// custody holding integrationCompleteTransferSetup mints) -- with
// caller-supplied tokenAddress/recipientAddress, using the well-known
// Wormhole devnet guardian key (the same one vectorgen_test.go signs every
// other fixture in this package with).
func signTokenBridgeTransferVAA(tokenAddress, recipientAddress [32]byte, sequence uint64) ([]byte, error) {
	priv, err := crypto.HexToECDSA("cfb12303a19cde580bb4dd771639b0d26bc68353645571a8cff516ab2ee113a0")
	if err != nil {
		return nil, err
	}

	be := func(n uint64, width int) []byte {
		b := make([]byte, width)
		full := make([]byte, 8)
		binary.BigEndian.PutUint64(full, n)
		if width >= 8 {
			copy(b[width-8:], full)
		} else {
			copy(b, full[8-width:])
		}
		return b
	}

	// Transfer payload, payload ID 1: id(1) ++ amount(32) ++ tokenAddress(32)
	// ++ tokenChain(2) ++ to(32) ++ toChain(2) ++ fee(32).
	payload := []byte{0x01}
	payload = append(payload, be(5_000_000_000, 32)...) // amount, 8dp
	payload = append(payload, tokenAddress[:]...)
	payload = append(payload, be(72, 2)...) // tokenChain (Canton)
	payload = append(payload, recipientAddress[:]...)
	payload = append(payload, be(72, 2)...) // toChain (Canton)
	payload = append(payload, be(0, 32)...) // fee

	// VAA body: emitter chain 2 (peerChainId), emitter = peer token-bridge
	// address 0x00..00beef (peerEmitterAddress).
	peerAddr := make([]byte, 32)
	peerAddr[31] = 0xef
	peerAddr[30] = 0xbe

	body := make([]byte, 0)
	ts := make([]byte, 4)
	binary.BigEndian.PutUint32(ts, 1700000000)
	body = append(body, ts...)      // timestamp
	body = append(body, 0, 0, 0, 0) // nonce 0
	body = append(body, 0x00, 0x02) // emitterChain 2
	body = append(body, peerAddr...)
	seqBytes := make([]byte, 8)
	binary.BigEndian.PutUint64(seqBytes, sequence)
	body = append(body, seqBytes...) // sequence
	body = append(body, 0x01)        // consistencyLevel
	body = append(body, payload...)

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

// completeTransferSetup mirrors Wormhole.Example.Test.TestTokenBridge's
// CompleteTransferSetup JSON shape.
type completeTransferSetup struct {
	CoreStateCid     string `json:"coreStateCid"`
	TbId             string `json:"tbId"`
	RegId            string `json:"regId"`
	CustodyHoldingId string `json:"custodyHoldingId"`
	Operator         string `json:"operator"`
	Bridge           string `json:"bridge"`
	Admin            string `json:"admin"`
	Recipient        string `json:"recipient"`
	TokenAddress     string `json:"tokenAddress"`
	RecipientAddress string `json:"recipientAddress"`
}

func TestCantonTokenBridgeCompleteIntegration(t *testing.T) {
	dpm := findDpm(t)

	cantonDir, err := filepath.Abs("../../../../canton")
	require.NoError(t, err)
	dar := filepath.Join(cantonDir, "examples", "token-bridge", ".daml", "dist",
		"wormhole-token-bridge-example-0.1.0.dar")

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

	// Step 1: allocate everything CompleteTransfer needs and get the ACTUAL,
	// on-ledger-computed tokenAddress/recipientAddress hashes.
	setupFile := filepath.Join(sandboxDir, "setup.json")
	out, err := exec.Command(dpm, "script", "--dar", dar, "--upload-dar", "yes",
		"--script-name", "Wormhole.Example.Test.TestTokenBridge:integrationCompleteTransferSetup",
		"--ledger-host", "localhost", "--ledger-port", port,
		"--output-file", setupFile).CombinedOutput()
	require.NoErrorf(t, err, "setup dpm script failed: %s", out)

	rawSetup, err := os.ReadFile(setupFile)
	require.NoError(t, err)
	var setup completeTransferSetup
	require.NoError(t, json.Unmarshal(rawSetup, &setup))
	require.NotEmpty(t, setup.Recipient)
	require.Contains(t, setup.Recipient, "::", "recipient should be a full party id")
	t.Logf("step 1: recipient=%s tokenAddress=%s recipientAddress=%s",
		setup.Recipient, setup.TokenAddress, setup.RecipientAddress)

	tokenAddrBytes, err := hex.DecodeString(strings.TrimPrefix(setup.TokenAddress, "0x"))
	require.NoError(t, err)
	require.Len(t, tokenAddrBytes, 32, "tokenAddressFor must be 32 bytes")
	var tokenAddr32 [32]byte
	copy(tokenAddr32[:], tokenAddrBytes)

	recipientAddrBytes, err := hex.DecodeString(strings.TrimPrefix(setup.RecipientAddress, "0x"))
	require.NoError(t, err)
	require.Len(t, recipientAddrBytes, 32, "tokenBridgeRecipientAddressFor must be 32 bytes")
	var recipientAddr32 [32]byte
	copy(recipientAddr32[:], recipientAddrBytes)

	// Step 2: sign a FRESH VAA, off-chain, against those ACTUAL hashes --
	// exactly what a real sender does.
	vaaBytes, err := signTokenBridgeTransferVAA(tokenAddr32, recipientAddr32, 1)
	require.NoError(t, err)
	t.Logf("step 2: signed a fresh VAA (%d bytes)", len(vaaBytes))

	// Step 3: relay it against the SAME sandbox/bridge/recipient from step 1.
	// All internal assertions (unlock lands on the matching recipient; replay
	// is rejected) live inside the script; a non-zero exit means one failed.
	inputPayload := fmt.Sprintf(
		`{"coreStateCid":%q,"tbId":%q,"regId":%q,"custodyHoldingId":%q,"operator":%q,"bridge":%q,"admin":%q,"recipient":%q,"vaaBytes":%q}`,
		setup.CoreStateCid, setup.TbId, setup.RegId, setup.CustodyHoldingId,
		setup.Operator, setup.Bridge, setup.Admin, setup.Recipient,
		hex.EncodeToString(vaaBytes),
	)
	inputFile := filepath.Join(sandboxDir, "input.json")
	require.NoError(t, os.WriteFile(inputFile, []byte(inputPayload), 0o600))

	out, err = exec.Command(dpm, "script", "--dar", dar,
		"--script-name", "Wormhole.Example.Test.TestTokenBridge:integrationCompleteTransfer",
		"--ledger-host", "localhost", "--ledger-port", port,
		"--input-file", inputFile).CombinedOutput()
	require.NoErrorf(t, err, "complete-transfer dpm script failed: %s", out)
	t.Logf("step 3: matching-recipient CompleteTransfer + replay rejection both verified live: %s", out)
}
