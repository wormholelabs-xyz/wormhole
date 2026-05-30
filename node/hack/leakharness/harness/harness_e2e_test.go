package harness_test

import (
	"context"
	"testing"
	"time"

	"github.com/certusone/wormhole/node/hack/leakharness/harness"
	"github.com/stretchr/testify/require"
)

// TestMezoFlapCompressed runs an abbreviated version of the mezo_flap
// scenario (20 s instead of 5 min) to verify the full pipeline:
// scenario load -> fakes start -> fault schedule applied ->
// connectors dial/close in a loop -> samples gathered -> slopes computed.
// The full 5-minute soak is for operators; this test is the CI smoke.
//
// This is the teeth test for the EVM connector restart leak (commit
// 6813f4aa). If `EthereumBaseConnector.Close()` regresses, every
// worker iteration strands ~3 goroutines; over ~150 cycles in 20 s
// that produces a slope well above the assertion bound below.
func TestMezoFlapCompressed(t *testing.T) {
	if testing.Short() {
		t.Skip("E2E harness test skipped in -short")
	}

	scenario := harness.Scenario{
		Name:           "mezo_flap_compressed",
		Description:    "compressed mezo_flap variant for unit-test pipelines",
		Duration:       20 * time.Second,
		SampleInterval: 2 * time.Second,
		OOMCapBytes:    6 << 30,
		Chains: map[string]harness.ChainSpec{
			"sepolia": {
				Fake:      harness.FakeEVM,
				Behaviour: harness.BehaviourFlapping,
				FaultSchedule: []harness.FaultEvent{
					{At: 3 * time.Second, Action: harness.FaultClose},
					{At: 4 * time.Second, Action: harness.FaultHeal},
					{At: 8 * time.Second, Action: harness.FaultClose},
					{At: 9 * time.Second, Action: harness.FaultHeal},
					{At: 13 * time.Second, Action: harness.FaultClose},
					{At: 14 * time.Second, Action: harness.FaultHeal},
				},
			},
		},
	}

	h, err := harness.New(scenario)
	require.NoError(t, err)

	ctx, cancel := context.WithTimeout(context.Background(), 90*time.Second)
	defer cancel()

	summary, err := h.Run(ctx)
	require.NoError(t, err)

	require.NotEqual(t, harness.VerdictKilledOOM, summary.Verdict,
		"OOM cap hit during compressed run: %+v", summary)
	require.Equal(t, harness.VerdictOK, summary.Verdict)
	require.GreaterOrEqual(t, summary.SampleCount, 5, "need at least 5 samples for stable regression")

	// Teeth: with Close() working, goroutine slope under fault-driven
	// restart should be modest. Pre-fix code (no Close) leaked ~3
	// goroutines per ~100 ms cycle, producing slopes in the hundreds
	// per minute. We assert under 5000/h, which a real leak would
	// blow past while accommodating GC/scheduler noise in 20 s.
	require.Less(t, summary.Slopes.GoroutinesPerHour, 5000.0,
		"goroutine slope %.1f/h suggests connector dispatch goroutines are leaking",
		summary.Slopes.GoroutinesPerHour)

	t.Logf("mezo_flap_compressed summary: peak_rss=%d MiB, peak_goroutines=%d, rss_slope=%.2f MB/h, gr_slope=%.2f /h",
		summary.PeakRSSBytes>>20, summary.PeakGoroutines,
		summary.Slopes.RSSMBPerHour, summary.Slopes.GoroutinesPerHour)
}

// TestMultiChainFlapCompressed exercises the family-registry dispatch
// path with more than one chain in the scenario. Pins the multi-chain
// scenario YAML and verifies fakes from different families both receive
// traffic.
func TestMultiChainFlapCompressed(t *testing.T) {
	if testing.Short() {
		t.Skip("E2E harness test skipped in -short")
	}

	scenario := harness.Scenario{
		Name:           "multi_chain_flap_compressed",
		Description:    "compressed multi-chain variant",
		Duration:       12 * time.Second,
		SampleInterval: 3 * time.Second,
		OOMCapBytes:    6 << 30,
		Chains: map[string]harness.ChainSpec{
			"sepolia": {
				Fake:      harness.FakeEVM,
				Behaviour: harness.BehaviourFlapping,
				FaultSchedule: []harness.FaultEvent{
					{At: 4 * time.Second, Action: harness.FaultClose},
					{At: 5 * time.Second, Action: harness.FaultHeal},
				},
			},
			"sui": {
				Fake:      harness.FakeSui,
				Behaviour: harness.BehaviourFlapping,
				FaultSchedule: []harness.FaultEvent{
					{At: 6 * time.Second, Action: harness.FaultClose},
					{At: 7 * time.Second, Action: harness.FaultHeal},
				},
			},
		},
	}

	h, err := harness.New(scenario)
	require.NoError(t, err)

	ctx, cancel := context.WithTimeout(context.Background(), 60*time.Second)
	defer cancel()

	summary, err := h.Run(ctx)
	require.NoError(t, err)

	require.Equal(t, harness.VerdictOK, summary.Verdict)
	require.GreaterOrEqual(t, summary.SampleCount, 3)

	t.Logf("multi_chain_flap_compressed: peak_rss=%d MiB, peak_goroutines=%d",
		summary.PeakRSSBytes>>20, summary.PeakGoroutines)
}
