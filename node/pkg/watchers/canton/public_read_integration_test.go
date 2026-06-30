//go:build integration

// End-to-end test that a read-only "public" observer party can read the
// core-bridge state over the real Ledger API v2 — proving public observability
// at the participant layer, not just in in-memory Daml Script.
//
// It boots a sandbox, creates the CoreState/Emitter/EmitterIdentity (all observed
// by a public party), then queries the active-contract set AS that public party
// through the real cantonclient and confirms all three durable templates are
// returned. Note: a dev sandbox is unauthenticated, so this exercises the
// Ledger-API observer/read path for the public party; the participant-side
// read-as-public authorization grant is a deployment concern (README §4.6).
//
//	go test -tags integration -run TestCantonPublicReadIntegration ./pkg/watchers/canton -v
//
// Requires `dpm` (PATH or ~/.dpm/bin) + a JDK; skipped otherwise. findDpm/runCmd
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

	"github.com/certusone/wormhole/node/pkg/cantonclient"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	"go.uber.org/zap"
	"google.golang.org/grpc"
	"google.golang.org/grpc/credentials/insecure"
)

func TestCantonPublicReadIntegration(t *testing.T) {
	dpm := findDpm(t)

	cantonDir, err := filepath.Abs("../../../../canton")
	require.NoError(t, err)
	// The test DAR carries Test.TestCore:integrationPublicParty and packs core.
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

	// Create CoreState + Emitter + EmitterIdentity (all observed by a public
	// party) and return that public party.
	partyFile := filepath.Join(sandboxDir, "public.json")
	out, err := exec.Command(dpm, "script", "--dar", dar, "--upload-dar", "yes",
		"--script-name", "Test.TestCore:integrationPublicParty",
		"--ledger-host", "localhost", "--ledger-port", port,
		"--output-file", partyFile).CombinedOutput()
	require.NoErrorf(t, err, "dpm script failed: %s", out)

	raw, err := os.ReadFile(partyFile)
	require.NoError(t, err)
	var publicParty string
	require.NoError(t, json.Unmarshal(raw, &publicParty))
	require.NotEmpty(t, publicParty)
	t.Logf("reading ACS as public party: %s", publicParty)

	// Read the ACS AS the public party through the real gRPC client.
	client, err := cantonclient.NewCantonGrpcClient(addr, publicParty, zap.NewNop(),
		grpc.WithTransportCredentials(insecure.NewCredentials()))
	require.NoError(t, err)
	defer client.Close()

	acs, err := client.GetActiveContracts(ctx)
	require.NoError(t, err)

	// The public party must see all three durable core-bridge templates
	// (all in module Wormhole.Core.State).
	seen := map[string]bool{}
	for _, ac := range acs {
		if ac.TemplateID.ModuleName == publishMessageModule {
			seen[ac.TemplateID.EntityName] = true
		}
	}
	assert.True(t, seen["CoreState"], "public party should read CoreState")
	assert.True(t, seen["Emitter"], "public party should read Emitter")
	assert.True(t, seen["EmitterIdentity"], "public party should read EmitterIdentity")
}
