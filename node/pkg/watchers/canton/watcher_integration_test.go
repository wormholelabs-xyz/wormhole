//go:build integration

// End-to-end integration test for the Canton watcher's real transport.
//
// It drives a live Canton sandbox via the `dpm` toolchain and observes a
// published Wormhole message through the cantonclient gRPC client, exercising
// the actual Ledger API v2 protos. Run:
//
//	go test -tags integration -run TestCantonWatcherIntegration ./pkg/watchers/canton -v
//
// Requires `dpm` (PATH or ~/.dpm/bin) + a JDK; skipped otherwise. Slow (boots a
// JVM sandbox), so it is excluded from the default build.
package canton

import (
	"context"
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
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	"github.com/wormhole-foundation/wormhole/sdk/vaa"
	"go.uber.org/zap"
	"google.golang.org/grpc"
	"google.golang.org/grpc/credentials/insecure"
)

func findDpm(t *testing.T) string {
	if p, err := exec.LookPath("dpm"); err == nil {
		return p
	}
	home, _ := os.UserHomeDir()
	p := filepath.Join(home, ".dpm", "bin", "dpm")
	if _, err := os.Stat(p); err == nil {
		return p
	}
	t.Skip("dpm not found (PATH or ~/.dpm/bin); skipping Canton integration test")
	return ""
}

func runCmd(t *testing.T, dir, name string, args ...string) {
	t.Helper()
	cmd := exec.Command(name, args...)
	cmd.Dir = dir
	if out, err := cmd.CombinedOutput(); err != nil {
		t.Fatalf("%s %v failed: %v\n%s", name, args, err, out)
	}
}

func TestCantonWatcherIntegration(t *testing.T) {
	dpm := findDpm(t)

	cantonDir, err := filepath.Abs("../../../../canton")
	require.NoError(t, err)
	// The test DAR carries Test.TestCore:integrationPublish and packs the core
	// DALFs (data-dependency), so --upload-dar uploads+vets core too.
	dar := filepath.Join(cantonDir, "test", ".daml", "dist", "wormhole-core-test-0.1.0.dar")

	port := os.Getenv("CANTON_SANDBOX_PORT")
	if port == "" {
		port = "6865"
	}
	addr := "localhost:" + port

	// Build both packages (core, then test).
	runCmd(t, cantonDir, dpm, "build", "--all")

	// Start the sandbox in its own process group (so we can kill the JVM child),
	// from a clean directory: `dpm sandbox` auto-loads a `*.canton` bootstrap or
	// daml.yaml init-script from its working dir, which we must avoid here.
	// Canton's sandbox binds the gRPC Ledger API on 6865 by default (no --port).
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

	// Wait for the sandbox to report ready (the gRPC + ledger are both up).
	require.Eventually(t, func() bool {
		b, _ := os.ReadFile(logPath)
		return strings.Contains(string(b), "Canton sandbox is ready")
	}, 180*time.Second, 2*time.Second, "sandbox never became ready")
	// Belt-and-suspenders: confirm the port accepts connections.
	conn, err := net.DialTimeout("tcp", addr, 5*time.Second)
	require.NoError(t, err)
	_ = conn.Close()

	// Run the integration script once: it uploads+vets the DAR (--upload-dar),
	// then sets up the bridge, registers an emitter, publishes one message, and
	// writes the Operator party id. --upload-dar makes vetting synchronous, so no
	// retry (and no non-idempotent re-allocation of the Operator party) is needed.
	partyFile := filepath.Join(sandboxDir, "party.json")
	out, err := exec.Command(dpm, "script", "--dar", dar, "--upload-dar", "yes",
		"--script-name", "Test.TestCore:integrationPublish",
		"--ledger-host", "localhost", "--ledger-port", port,
		"--output-file", partyFile).CombinedOutput()
	require.NoErrorf(t, err, "dpm script failed: %s", out)

	raw, err := os.ReadFile(partyFile)
	require.NoError(t, err)
	var operator string
	require.NoError(t, json.Unmarshal(raw, &operator))
	require.NotEmpty(t, operator)
	t.Logf("published as operator party: %s", operator)

	// Observe through the REAL cantonclient gRPC client (insecure, dev sandbox).
	// Empty readAsParty => wildcard "any party" filter, so we observe the message
	// without knowing the (namespace-fingerprinted) operator party id.
	client, err := cantonclient.NewCantonGrpcClient(addr, "", zap.NewNop(),
		grpc.WithTransportCredentials(insecure.NewCredentials()))
	require.NoError(t, err)
	defer client.Close()

	eventChan := make(chan cantonclient.CantonMessageEvent, 8)
	tmpl := cantonclient.TemplateID{ModuleName: publishMessageModule, EntityName: publishMessageEntity}
	// beginExclusive=0 streams from the start, so the already-published message is delivered.
	sub, err := client.SubscribeUpdates(ctx, 0, tmpl, publishMessageChoice, eventChan)
	require.NoError(t, err)
	defer sub.Unsubscribe()

	select {
	case ev := <-eventChan:
		// The decoded event from the live ledger. The address is derived from the
		// emitter's identity components (registrar/owner/emitterId) assigned by
		// the live ledger (integrationPublish exercises the Emitter by cid).
		require.NotEmpty(t, ev.Message.Registrar)
		require.NotEmpty(t, ev.Message.Owner)
		require.Contains(t, ev.Message.Registrar, "::", "registrar should be a full party id")
		assert.Equal(t, uint64(0), ev.Message.EmitterID)
		assert.Equal(t, uint64(0), ev.Message.Sequence)
		assert.Equal(t, uint32(42), ev.Message.Nonce)
		assert.Equal(t, []byte{0x11, 0x22, 0x33, 0x44}, ev.Message.Payload)

		wantAddr := deriveEmitterAddress(ev.Message.Registrar, ev.Message.Owner, ev.Message.EmitterID)
		assert.NotEqual(t, vaa.Address{}, wantAddr, "derived emitter address must be non-zero")

		// Feed it through the watcher to confirm the MessagePublication mapping.
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
