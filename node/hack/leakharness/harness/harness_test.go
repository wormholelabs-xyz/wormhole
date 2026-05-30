// Package harness contains the leak-detection harness orchestrator.
//
// This test file is Phase 0 of the plan at
// `.claude/tasks/guardian-leak-harness.md`. It expresses the public API
// contract that Phase 1 must satisfy. On Phase 0 the harness package
// has no implementation, so this test fails to build — that compile
// failure is the failing red bar TDD requires.
package harness_test

import (
	"context"
	"testing"
	"time"

	"github.com/certusone/wormhole/node/hack/leakharness/harness"
	"github.com/stretchr/testify/require"
)

// TestSteadyStateOnHEAD runs the steady-state scenario against the
// current guardiand HEAD and asserts that the harness completes without
// hitting the 6 GiB OOM cap. It does NOT assert any slope bound — the
// harness is report-only by decision #6 of the plan; the slopes are
// logged so an operator can eyeball them.
func TestSteadyStateOnHEAD(t *testing.T) {
	if testing.Short() {
		t.Skip("leakharness E2E test takes minutes; skipped in -short")
	}

	scenario, err := harness.LoadScenario("../scenarios/steady_state.yaml")
	require.NoError(t, err, "loading scenario yaml")

	// Allow scenario.Duration plus generous slack for guardiand build,
	// boot, scrape settling and teardown.
	ctx, cancel := context.WithTimeout(context.Background(), scenario.Duration+2*time.Minute)
	defer cancel()

	h, err := harness.New(scenario)
	require.NoError(t, err, "constructing harness")

	summary, err := h.Run(ctx)
	require.NoError(t, err, "running harness")

	require.NotEqual(t, harness.VerdictKilledOOM, summary.Verdict,
		"guardiand hit 6 GiB RSS cap during steady-state scenario; slopes captured up to kill: %+v", summary.Slopes)

	t.Logf("steady-state slopes — RSS: %.2f MB/h, heap_inuse: %.2f MB/h, goroutines: %.2f/h, fds: %.2f/h",
		summary.Slopes.RSSMBPerHour,
		summary.Slopes.HeapInuseMBPerHour,
		summary.Slopes.GoroutinesPerHour,
		summary.Slopes.FDsPerHour)
}
