//go:build integration

// End-to-end integration test for the Canton watcher: it boots a real Canton
// sandbox via `dpm`, runs the actual watcher.Run over the Ledger API v2 gRPC
// transport, and asserts live observation, height advance, and reobservation.
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

	"github.com/certusone/wormhole/node/pkg/common"
	gossipv1 "github.com/certusone/wormhole/node/pkg/proto/gossip/v1"
	"github.com/certusone/wormhole/node/pkg/supervisor"
	"github.com/prometheus/client_golang/prometheus/testutil"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	"github.com/wormhole-foundation/wormhole/sdk/vaa"
	"go.uber.org/zap"
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

// startSandbox builds the Daml packages and starts a Canton sandbox bound to
// the ctx lifetime (the JVM child is killed when ctx is cancelled), returning
// the dpm binary, the test DAR path, and the ledger port.
func startSandbox(t *testing.T, ctx context.Context) (dpm, dar, port string) {
	t.Helper()
	dpm = findDpm(t)

	cantonDir, err := filepath.Abs("../../../../canton")
	require.NoError(t, err)
	// The test DAR carries Test.TestCore:integrationPublish and packs the core
	// DALFs (data-dependency), so --upload-dar uploads+vets core too.
	dar = filepath.Join(cantonDir, "test", ".daml", "dist", "wormhole-core-test-0.1.0.dar")

	port = os.Getenv("CANTON_SANDBOX_PORT")
	if port == "" {
		port = "6865"
	}

	// Build both packages (core, then test).
	runCmd(t, cantonDir, dpm, "build", "--all")

	// Start the sandbox in its own process group (so we can kill the JVM child),
	// from a clean directory: `dpm sandbox` auto-loads a `*.canton` bootstrap or
	// daml.yaml init-script from its working dir, which we must avoid here.
	// Canton's sandbox binds the gRPC Ledger API on 6865 by default (no --port).
	sandboxDir := t.TempDir()
	logPath := filepath.Join(sandboxDir, "sandbox.log")
	logFile, err := os.Create(logPath)
	require.NoError(t, err)
	t.Cleanup(func() { _ = logFile.Close() })

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
	conn, err := net.DialTimeout("tcp", "localhost:"+port, 5*time.Second)
	require.NoError(t, err)
	_ = conn.Close()
	return dpm, dar, port
}

// publishWormholeMessage runs Test.TestCore:integrationPublish once, which sets
// up the bridge, registers an emitter, and publishes one WormholeMessage
// (nonce 42, payload 0x11223344). --upload-dar makes vetting synchronous.
func publishWormholeMessage(t *testing.T, dpm, dar, port string) {
	t.Helper()
	partyFile := filepath.Join(t.TempDir(), "party.json")
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
}

func TestCantonWatcherIntegration(t *testing.T) {
	rootCtx, cancel := context.WithTimeout(context.Background(), 5*time.Minute)
	defer cancel()

	dpm, dar, port := startSandbox(t, rootCtx)
	addr := "localhost:" + port

	// Buffered so the watcher never blocks publishing; the test drains it.
	msgC := make(chan *common.MessagePublication, 16)
	obsvReqC := make(chan *gossipv1.ObservationRequest, 4)

	// packageID "" ⇒ match any package version; readAsParty "" ⇒ observe all
	// parties (wildcard); unsafeDevMode true ⇒ insecure gRPC to the local
	// sandbox.
	w := NewWatcher(addr, "", "", true, msgC, obsvReqC)

	// Start the real watcher exactly as guardiand does.
	supervisor.New(rootCtx, zap.NewNop(), func(ctx context.Context) error {
		if err := supervisor.Run(ctx, "canton", w.Run); err != nil {
			return err
		}
		<-ctx.Done()
		return nil
	}, supervisor.WithPropagatePanic)

	// The data pump streams from the ledger end captured at Run start, so a
	// message published now (after the watcher started) is delivered even if the
	// subscription connects slightly later.
	publishWormholeMessage(t, dpm, dar, port)

	// 1. Live observation through the running watcher.
	var first *common.MessagePublication
	select {
	case first = <-msgC:
	case <-time.After(90 * time.Second):
		t.Fatal("watcher did not emit the published message within 90s")
	}
	assert.False(t, first.IsReobservation, "first observation must not be flagged as a reobservation")
	assert.Equal(t, vaa.ChainIDCanton, first.EmitterChain)
	assert.Equal(t, uint64(0), first.Sequence)
	assert.Equal(t, uint32(42), first.Nonce)
	assert.Equal(t, []byte{0x11, 0x22, 0x33, 0x44}, first.Payload)
	assert.NotEqual(t, vaa.Address{}, first.EmitterAddress, "derived emitter address must be non-zero")
	require.Len(t, first.TxID, 32, "TxID is the 32-byte encoded participant offset")

	// 2. The block-height goroutine reports a non-zero ledger offset (the
	//    readiness/height path), which advances once messages exist.
	require.Eventually(t, func() bool {
		return testutil.ToFloat64(currentCantonHeight) > 0
	}, 15*time.Second, time.Second, "height gauge never advanced")

	// 3. Reobservation over the wire: a gossip ObservationRequest for the same
	//    TxID drives GetUpdateByOffset and re-emits the message.
	obsvReqC <- &gossipv1.ObservationRequest{
		ChainId: uint32(vaa.ChainIDCanton),
		TxHash:  first.TxID,
	}
	var reobs *common.MessagePublication
	select {
	case reobs = <-msgC:
	case <-time.After(60 * time.Second):
		t.Fatal("watcher did not re-emit the message on reobservation within 60s")
	}
	assert.True(t, reobs.IsReobservation, "second emission must be flagged as a reobservation")
	assert.Equal(t, vaa.ChainIDCanton, reobs.EmitterChain)
	assert.Equal(t, first.Sequence, reobs.Sequence)
	assert.Equal(t, first.Nonce, reobs.Nonce)
	assert.Equal(t, first.Payload, reobs.Payload)
	assert.Equal(t, first.EmitterAddress, reobs.EmitterAddress)
	assert.Equal(t, first.TxID, reobs.TxID)
}
