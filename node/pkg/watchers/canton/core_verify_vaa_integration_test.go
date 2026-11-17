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
// a JVM sandbox), so it is excluded from the default build. startSandbox is
// shared with watcher_integration_test.go.
package canton

import (
	"context"
	"encoding/json"
	"os"
	"os/exec"
	"path/filepath"
	"testing"

	"github.com/stretchr/testify/require"
)

func TestCantonParseAndVerifyVAAExternalVerifierIntegration(t *testing.T) {
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	dpm, dar, port := startSandbox(t, ctx)

	// Upload+vet the DAR and run the scenario. All the real assertions (the
	// verification succeeding, the decoded VAA fields matching) live inside the
	// script; a non-zero exit here means one of them failed.
	partyFile := filepath.Join(t.TempDir(), "verifier.json")
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
