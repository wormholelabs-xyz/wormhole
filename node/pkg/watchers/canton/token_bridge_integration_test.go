//go:build integration

// End-to-end integration test for the token-bridge lock-and-attest send path on
// the Canton Network Token Standard (CIP-0056).
//
// It boots a live Canton sandbox, uploads+vets the example DAR (which packs the
// vendored splice-api-token-* DALFs, wormhole-core, and the example), runs the
// lock-and-attest flow via `dpm script`, and observes the resulting Wormhole
// Transfer message through the real cantonclient gRPC client. This additionally
// proves what the in-memory Daml Script test cannot: that the SDK-3.3.x-built
// token-standard DARs vet on the 3.4.x sandbox, that explicit disclosure works
// over the real Ledger API, and that a token-custody lock produces a message the
// guardian watcher maps to a common.MessagePublication.
//
//	go test -tags integration -run TestCantonTokenBridgeIntegration ./pkg/watchers/canton -v
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
	"net"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"syscall"
	"testing"
	"time"

	"github.com/certusone/wormhole/node/pkg/cantonclient"
	"github.com/certusone/wormhole/node/pkg/common"
	gossipv1 "github.com/certusone/wormhole/node/pkg/proto/gossip/v1"
	ethcrypto "github.com/ethereum/go-ethereum/crypto"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	"github.com/wormhole-foundation/wormhole/sdk/vaa"
	"go.uber.org/zap"
	"google.golang.org/grpc"
	"google.golang.org/grpc/credentials/insecure"
)

func TestCantonTokenBridgeIntegration(t *testing.T) {
	dpm := findDpm(t)

	cantonDir, err := filepath.Abs("../../../../canton")
	require.NoError(t, err)
	// The example DAR packs the splice-api-token-* DALFs, wormhole-core, and the
	// example itself, so --upload-dar uploads+vets the whole set in one shot.
	dar := filepath.Join(cantonDir, "examples", "token-bridge", ".daml", "dist",
		"wormhole-token-bridge-example-0.1.0.dar")

	port := os.Getenv("CANTON_SANDBOX_PORT")
	if port == "" {
		port = "6865"
	}
	addr := "localhost:" + port

	// Build all packages (core, test, example).
	runCmd(t, cantonDir, dpm, "build", "--all")

	// Start the sandbox in its own process group, from a clean directory (so
	// `dpm sandbox` does not auto-load a bootstrap/init-script). Ledger API on 6865.
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

	// Upload+vet the example DAR and run the lock-and-attest flow. --upload-dar
	// makes vetting synchronous, so no retry is needed.
	partyFile := filepath.Join(sandboxDir, "party.json")
	out, err := exec.Command(dpm, "script", "--dar", dar, "--upload-dar", "yes",
		"--script-name", "Wormhole.Example.Test.TestTokenBridge:integrationLockAndPublish",
		"--ledger-host", "localhost", "--ledger-port", port,
		"--output-file", partyFile).CombinedOutput()
	require.NoErrorf(t, err, "dpm script failed: %s", out)

	raw, err := os.ReadFile(partyFile)
	require.NoError(t, err)
	var bridge string
	require.NoError(t, json.Unmarshal(raw, &bridge))
	require.NotEmpty(t, bridge)
	t.Logf("locked + attested; bridge party: %s", bridge)

	// Observe through the REAL cantonclient gRPC client. Empty readAsParty =>
	// wildcard "any party" filter, so we observe without the party fingerprint.
	client, err := cantonclient.NewCantonGrpcClient(addr, "", zap.NewNop(),
		grpc.WithTransportCredentials(insecure.NewCredentials()))
	require.NoError(t, err)
	defer client.Close()

	eventChan := make(chan cantonclient.CantonMessageEvent, 8)
	tmpl := cantonclient.TemplateID{ModuleName: publishMessageModule, EntityName: publishMessageEntity}
	sub, err := client.SubscribeUpdates(ctx, 0, tmpl, publishMessageChoice, eventChan)
	require.NoError(t, err)
	defer sub.Unsubscribe()

	select {
	case ev := <-eventChan:
		// Header: the emitter address is keccak256 of the emitter's identity
		// contract-id, recomputed here from the cid the live ledger assigned.
		require.NotEmpty(t, ev.Message.IdentityCID)
		assert.Equal(t, uint64(0), ev.Message.Sequence)
		assert.Equal(t, uint32(42), ev.Message.Nonce)

		cidBytes, err := hex.DecodeString(strings.TrimPrefix(ev.Message.IdentityCID, "0x"))
		require.NoError(t, err)
		var wantAddr vaa.Address
		copy(wantAddr[:], ethcrypto.Keccak256(cidBytes))
		assert.NotEqual(t, vaa.Address{}, wantAddr, "derived emitter address must be non-zero")

		// Payload: Wormhole TokenBridge Transfer, payloadID 1 (133 bytes):
		// id(1) ++ amount(32) ++ token(32) ++ tokenChain(2) ++ to(32) ++ toChain(2) ++ fee(32).
		p := ev.Message.Payload
		require.Len(t, p, 133)
		assert.Equal(t, byte(0x01), p[0], "payloadID")
		assert.Equal(t, uint64(1000), binary.BigEndian.Uint64(p[25:33]), "amount")
		assert.Equal(t, uint16(72), binary.BigEndian.Uint16(p[65:67]), "tokenChain")
		assert.Equal(t, byte(0xaa), p[97], "recipient")
		assert.Equal(t, byte(0xaa), p[98], "recipient")
		assert.Equal(t, uint16(2), binary.BigEndian.Uint16(p[99:101]), "toChain")

		// Feed through the watcher to confirm the MessagePublication mapping.
		msgC := make(chan *common.MessagePublication, 1)
		w := NewWatcher(addr, "", "", true, msgC, make(chan *gossipv1.ObservationRequest))
		w.processMessage(zap.NewNop(), ev, false)
		mp := <-msgC
		assert.Equal(t, vaa.ChainIDCanton, mp.EmitterChain)
		assert.Equal(t, uint64(0), mp.Sequence)
		assert.Equal(t, uint32(42), mp.Nonce)
		assert.Equal(t, wantAddr, mp.EmitterAddress)
		assert.Equal(t, cantonclient.OffsetToTxID(ev.Offset), mp.TxID)
	case err := <-sub.Err():
		t.Fatalf("subscription error: %v", err)
	case <-time.After(60 * time.Second):
		t.Fatal("did not observe the published Wormhole message within 60s")
	}
}
