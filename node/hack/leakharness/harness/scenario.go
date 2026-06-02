package harness

import (
	"fmt"
	"os"
	"path/filepath"
	"time"

	"gopkg.in/yaml.v3"
)

// Behaviour names a fake-RPC operating mode. Phase 1 ships only
// "healthy"; Phase 2 adds "flapping", "slow", "stuck", "malformed".
type Behaviour string

const (
	BehaviourHealthy   Behaviour = "healthy"
	BehaviourFlapping  Behaviour = "flapping"
	BehaviourSlow      Behaviour = "slow"
	BehaviourStuck     Behaviour = "stuck"
	BehaviourMalformed Behaviour = "malformed"
)

// FakeKind names the fake-RPC family for a chain.
type FakeKind string

const (
	FakeEVM FakeKind = "evm"
	// FakeEVMLeak is the EVM family with a deliberately leaking worker:
	// it dials connectors and abandons them WITHOUT Close, modelling the
	// pre-fix supervisor restart. Used by the self-test scenario to prove
	// the harness actually detects the leak it was built for.
	FakeEVMLeak  FakeKind = "evm_leak"
	FakeSui      FakeKind = "sui"
	FakeCosmwasm FakeKind = "cosmwasm"
	FakeXRPL     FakeKind = "xrpl"
)

// FaultAction is a single discrete fault to inject at a specific
// scenario time. Phase 1 declares the schema but only "noop" is wired.
type FaultAction string

const (
	FaultClose      FaultAction = "close_websockets"
	FaultHeal       FaultAction = "heal"
	FaultFreeze     FaultAction = "freeze_heights"
	FaultMalformed  FaultAction = "return_malformed"
	FaultSlow       FaultAction = "slow_response"
	FaultDisconnect FaultAction = "disconnect"
)

type FaultEvent struct {
	At     time.Duration `yaml:"at"`
	Action FaultAction   `yaml:"action"`
}

type ChainSpec struct {
	Fake          FakeKind     `yaml:"fake"`
	Behaviour     Behaviour    `yaml:"behaviour"`
	FaultSchedule []FaultEvent `yaml:"fault_schedule,omitempty"`
}

type GuardianSpec struct {
	UnsafeDevMode bool     `yaml:"unsafe_dev_mode"`
	EnabledChains []string `yaml:"enabled_chains"`
	Signer        string   `yaml:"signer"`
	ExtraFlags    []string `yaml:"extra_flags"`
}

type SlopeReport struct {
	RSSMBPerHour       bool `yaml:"rss_mb_per_hour"`
	HeapInuseMBPerHour bool `yaml:"heap_inuse_mb_per_hour"`
	GoroutinesPerHour  bool `yaml:"goroutines_per_hour"`
	FDsPerHour         bool `yaml:"fds_per_hour"`
}

// Scenario is the full parsed scenario. The Defaults string references
// a sibling YAML whose top-level fields are merged in if the scenario
// itself does not set them.
type Scenario struct {
	Name           string               `yaml:"name"`
	Description    string               `yaml:"description"`
	Duration       time.Duration        `yaml:"duration"`
	SampleInterval time.Duration        `yaml:"sample_interval"`
	Guardian       GuardianSpec         `yaml:"guardian"`
	OOMCapBytes    uint64               `yaml:"oom_cap_bytes"`
	// MaxGoroutineGrowth, when > 0, turns the run into a hard gate: if the
	// GC-settled goroutine delta (end - start) exceeds it, the verdict is
	// leak_detected. Zero (default) means report-only.
	MaxGoroutineGrowth int                  `yaml:"max_goroutine_growth"`
	SlopeReport        SlopeReport          `yaml:"slope_report"`
	Defaults           string               `yaml:"defaults"`
	Chains             map[string]ChainSpec `yaml:"chains"`
}

// LoadScenario reads `path` and, if it declares a `defaults:` field,
// merges the sibling defaults file into any fields the scenario itself
// did not set. The defaults file is resolved relative to `path`.
func LoadScenario(path string) (Scenario, error) {
	primary, err := readScenarioFile(path)
	if err != nil {
		return Scenario{}, err
	}

	if primary.Defaults == "" {
		return primary, validate(primary)
	}

	defaultsPath := filepath.Join(filepath.Dir(path), primary.Defaults)
	defaults, err := readScenarioFile(defaultsPath)
	if err != nil {
		return Scenario{}, fmt.Errorf("loading defaults %q: %w", defaultsPath, err)
	}

	merged := mergeScenarios(defaults, primary)
	return merged, validate(merged)
}

func readScenarioFile(path string) (Scenario, error) {
	data, err := os.ReadFile(path)
	if err != nil {
		return Scenario{}, fmt.Errorf("reading %q: %w", path, err)
	}
	var s Scenario
	if err := yaml.Unmarshal(data, &s); err != nil {
		return Scenario{}, fmt.Errorf("parsing %q: %w", path, err)
	}
	return s, nil
}

// mergeScenarios overlays `primary` onto `defaults`. Any field set on
// `primary` wins; otherwise the value from `defaults` is used.
//
// Caveat: the merge treats Go zero values as "unset". A scenario that
// intentionally sets a bool to false or a duration to zero cannot
// override a non-zero default. This is acceptable for the current
// schema (no scenario wants to disable a default flag) but should be
// revisited if knobs like `unsafe_dev_mode: false` ever become
// load-bearing — at that point switch the affected fields to *bool /
// *time.Duration pointers.
func mergeScenarios(defaults, primary Scenario) Scenario {
	out := defaults

	if primary.Name != "" {
		out.Name = primary.Name
	}
	if primary.Description != "" {
		out.Description = primary.Description
	}
	if primary.Duration != 0 {
		out.Duration = primary.Duration
	}
	if primary.SampleInterval != 0 {
		out.SampleInterval = primary.SampleInterval
	}
	if primary.OOMCapBytes != 0 {
		out.OOMCapBytes = primary.OOMCapBytes
	}
	if primary.MaxGoroutineGrowth != 0 {
		out.MaxGoroutineGrowth = primary.MaxGoroutineGrowth
	}
	// Guardian: any non-zero field on primary wins.
	if primary.Guardian.UnsafeDevMode {
		out.Guardian.UnsafeDevMode = true
	}
	if len(primary.Guardian.EnabledChains) > 0 {
		out.Guardian.EnabledChains = primary.Guardian.EnabledChains
	}
	if primary.Guardian.Signer != "" {
		out.Guardian.Signer = primary.Guardian.Signer
	}
	if len(primary.Guardian.ExtraFlags) > 0 {
		out.Guardian.ExtraFlags = primary.Guardian.ExtraFlags
	}
	// SlopeReport: bools, if any primary bool is set, the whole struct
	// is overridden. Simpler than fine-grained merge.
	if primary.SlopeReport != (SlopeReport{}) {
		out.SlopeReport = primary.SlopeReport
	}
	if len(primary.Chains) > 0 {
		out.Chains = primary.Chains
	}
	return out
}

func validate(s Scenario) error {
	if s.Name == "" {
		return fmt.Errorf("scenario.name is required")
	}
	if s.Duration <= 0 {
		return fmt.Errorf("scenario.duration must be > 0")
	}
	if s.SampleInterval <= 0 {
		return fmt.Errorf("scenario.sample_interval must be > 0 (defaults file may not have been merged)")
	}
	if s.OOMCapBytes == 0 {
		return fmt.Errorf("scenario.oom_cap_bytes must be > 0")
	}
	if len(s.Chains) == 0 {
		return fmt.Errorf("scenario.chains must declare at least one chain")
	}
	for name, c := range s.Chains {
		if c.Fake == "" {
			return fmt.Errorf("chains[%s].fake is required", name)
		}
		if c.Behaviour == "" {
			return fmt.Errorf("chains[%s].behaviour is required", name)
		}
	}
	return nil
}

