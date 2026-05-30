package harness

import (
	"os"
	"path/filepath"
	"testing"
	"time"

	"github.com/stretchr/testify/require"
)

func TestLoadScenario_MergesDefaults(t *testing.T) {
	tmp := t.TempDir()

	defaultsYAML := []byte(`sample_interval: 30s
guardian:
  enabled_chains: [sepolia]
oom_cap_bytes: 6442450944
slope_report:
  rss_mb_per_hour: true
`)
	require.NoError(t, os.WriteFile(filepath.Join(tmp, "_defaults.yaml"), defaultsYAML, 0o644))

	scenarioYAML := []byte(`name: example
description: a test scenario
duration: 30s
defaults: _defaults.yaml
chains:
  sepolia:
    fake: evm
    behaviour: healthy
`)
	scenarioPath := filepath.Join(tmp, "example.yaml")
	require.NoError(t, os.WriteFile(scenarioPath, scenarioYAML, 0o644))

	scenario, err := LoadScenario(scenarioPath)
	require.NoError(t, err)

	require.Equal(t, "example", scenario.Name)
	require.Equal(t, 30*time.Second, scenario.Duration)
	require.Equal(t, 30*time.Second, scenario.SampleInterval, "sample_interval should come from defaults")
	require.Equal(t, uint64(6442450944), scenario.OOMCapBytes, "oom_cap_bytes should come from defaults")
	require.Equal(t, []string{"sepolia"}, scenario.Guardian.EnabledChains)
	require.True(t, scenario.SlopeReport.RSSMBPerHour)
	require.Len(t, scenario.Chains, 1)
}

func TestLoadScenario_RejectsMissingFields(t *testing.T) {
	tmp := t.TempDir()

	scenarioYAML := []byte(`name: malformed
duration: 30s
chains:
  sepolia:
    fake: evm
    behaviour: healthy
`)
	path := filepath.Join(tmp, "bad.yaml")
	require.NoError(t, os.WriteFile(path, scenarioYAML, 0o644))

	_, err := LoadScenario(path)
	require.Error(t, err, "missing sample_interval should fail validation")
}

func TestLoadScenario_RealSteadyStateYAML(t *testing.T) {
	// Exercise the actual scenarios/steady_state.yaml shipped in the
	// harness so a typo there fails this test, not the long E2E run.
	scenario, err := LoadScenario("../scenarios/steady_state.yaml")
	require.NoError(t, err)
	require.Equal(t, "steady_state", scenario.Name)
	require.True(t, scenario.Duration > 0)
	require.True(t, scenario.SampleInterval > 0)
	require.True(t, scenario.OOMCapBytes > 0)
	require.NotEmpty(t, scenario.Chains)
}

func TestLoadScenario_RealMezoFlapYAML(t *testing.T) {
	scenario, err := LoadScenario("../scenarios/mezo_flap.yaml")
	require.NoError(t, err)
	require.Equal(t, "mezo_flap", scenario.Name)
	require.Equal(t, 5*time.Minute, scenario.Duration)
	require.NotEmpty(t, scenario.Chains["sepolia"].FaultSchedule)
}
