//go:build integration

// Integration test proving ParseAndVerifyVAA is callable by an external
// "verifier" party that is NOT a stakeholder of CoreState (CoreState's only
// stakeholders are operator and guardianGovernance), against a live Canton
// sandbox/participant -- not just the Script interpreter.
//
// It runs Test.TestCore:integrationParseAndVerifyVAAByExternalVerifier, which
// allocates a fresh external party, has CoreState explicitly disclosed to it
// (a data attachment, not a signature), and verifies a real guardian-signed
// VAA as that party alone -- operator is never an active party in that
// submission. The script's own assertions (guardianSetIndex/emitterChain/
// sequence checks) make `dpm script` exit non-zero if verification failed;
// this test additionally confirms the returned party is a real, live-hosted
// party id, not something forged.
//
//	go test -tags integration -run TestCantonParseAndVerifyVAAExternalVerifierIntegration ./pkg/watchers/canton -v
//
// Requires `dpm` (PATH or ~/.dpm/bin) + a JDK; skipped otherwise. Slow (boots
// a JVM sandbox), so it is excluded from the default build. findDpm/runCmd
// are shared with watcher_integration_test.go.
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

	"github.com/stretchr/testify/require"
)

func TestCantonParseAndVerifyVAAExternalVerifierIntegration(t *testing.T) {
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

	// Upload+vet the DAR and run the scenario. All the real assertions (the
	// verification succeeding, the decoded VAA fields matching) live inside the
	// script; a non-zero exit here means one of them failed.
	partyFile := filepath.Join(sandboxDir, "verifier.json")
	out, err := exec.Command(dpm, "script", "--dar", dar, "--upload-dar", "yes",
		"--script-name", "Test.TestCore:integrationParseAndVerifyVAAByExternalVerifier",
		"--ledger-host", "localhost", "--ledger-port", port,
		"--output-file", partyFile).CombinedOutput()
	require.NoErrorf(t, err, "dpm script failed: %s", out)

	raw, err := os.ReadFile(partyFile)
	require.NoError(t, err)
	var verifier string
	require.NoError(t, json.Unmarshal(raw, &verifier))
	require.NotEmpty(t, verifier)
	require.Contains(t, verifier, "::", "verifier should be a full, live-hosted party id")
	t.Logf("external, non-stakeholder verifier party successfully called ParseAndVerifyVAA: %s", verifier)
}
