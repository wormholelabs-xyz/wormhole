// Package harness orchestrates leak-detection scenarios against the
// guardian watcher code in-process. See the package README for the design
// and for how to read the count-based leak signal vs the report-only slopes.
//
// The harness exercises the lifecycle that production leaks live in:
//
//   dial connector -> use connector -> close connector -> repeat
//
// faulted by an injectable fake-RPC. It does NOT spawn guardiand as a
// subprocess; that fidelity is deferred to a Phase 5 follow-up. The
// in-process approach catches the connector-restart leak class
// (sui hot-loop, evm restart) that motivated this harness, without the
// 40-flag guardiand startup overhead.
package harness

import (
	"context"
	"encoding/json"
	"fmt"
	"hash/fnv"
	"os"
	"path/filepath"
	"runtime"
	"strings"
	"sync"
	"time"

	"go.uber.org/zap"

	"github.com/certusone/wormhole/node/hack/leakharness/fakerpc/common"
	"github.com/certusone/wormhole/node/hack/leakharness/fakerpc/cosmwasm"
	"github.com/certusone/wormhole/node/hack/leakharness/fakerpc/evm"
	"github.com/certusone/wormhole/node/hack/leakharness/fakerpc/sui"
	"github.com/certusone/wormhole/node/hack/leakharness/fakerpc/xrpl"
)

// familyRegistry maps a FakeKind to the chain-family hooks the harness
// needs. New chain families register themselves here.
var familyRegistry = map[FakeKind]common.Family{
	FakeEVM:      evm.Family,
	FakeEVMLeak:  evm.LeakFamily,
	FakeSui:      sui.Family,
	FakeCosmwasm: cosmwasm.Family,
	FakeXRPL:     xrpl.Family,
}

// Verdict labels the outcome of a harness run.
type Verdict string

const (
	VerdictOK        Verdict = "ok"
	VerdictKilledOOM Verdict = "killed_oom"
	VerdictAborted   Verdict = "aborted"
	// VerdictLeak is returned when a scenario sets max_goroutine_growth
	// and the GC-settled goroutine delta exceeds it. This is the
	// deterministic gate; the per-hour Slopes are report-only.
	VerdictLeak Verdict = "leak_detected"
)

// Summary is the JSON-serialisable record of a single scenario run.
type Summary struct {
	Scenario       string    `json:"scenario"`
	Description    string    `json:"description"`
	StartedAt      time.Time `json:"started_at"`
	CompletedAt    time.Time `json:"completed_at"`
	SampleCount    int       `json:"sample_count"`
	Verdict        Verdict   `json:"verdict"`
	OOMCapBytes    uint64    `json:"oom_cap_bytes"`
	PeakRSSBytes   uint64    `json:"peak_rss_bytes"`
	PeakGoroutines int       `json:"peak_goroutines"`
	// Counts is the primary, deterministic leak signal: a GC-settled
	// census of reachable heap objects and goroutines at scenario start
	// vs end. Prefer this over Slopes — RSS slope is OS-level noise.
	Counts CountDeltas `json:"counts"`
	// TopGoroutineGrowth names the goroutine stacks that accumulated the
	// most over the run. For an unclosed-connector leak this points
	// straight at the leaked rpc.Client dispatch/read/write goroutines.
	TopGoroutineGrowth []StackGrowth `json:"top_goroutine_growth,omitempty"`
	// Slopes are retained for trend context but are report-only: at short
	// durations and small footprints the RSS slope variance swamps any
	// leak signal (see README). Do not gate on them.
	Slopes Slopes `json:"slopes"`
	// Profiles, when present, holds the relative paths to pprof
	// profiles captured at scenario start and end. Empty when the
	// harness was constructed without a ProfileDir.
	Profiles ProfilePaths `json:"profiles,omitempty"`
}

// ProfilePaths records the relative paths of pprof profiles captured
// during a Run.
type ProfilePaths struct {
	HeapStart      string `json:"heap_start,omitempty"`
	HeapEnd        string `json:"heap_end,omitempty"`
	GoroutineStart string `json:"goroutine_start,omitempty"`
	GoroutineEnd   string `json:"goroutine_end,omitempty"`
}

// Harness is one configured run, reusable across multiple Run() calls
// only by reconstructing.
type Harness struct {
	scenario Scenario
	logger   *zap.Logger

	// ProfileDir, when non-empty, causes Run() to write heap and
	// goroutine pprof profiles at scenario start and end. The
	// resulting `heap-start.pprof` / `heap-end.pprof` pair is the
	// fastest way to localise an allocation regression:
	//   go tool pprof -base heap-start.pprof heap-end.pprof
	ProfileDir string
}

// New constructs a Harness for the given scenario. It does not start
// any work; call Run to drive the scenario.
func New(scenario Scenario) (*Harness, error) {
	logger, err := zap.NewDevelopment()
	if err != nil {
		return nil, fmt.Errorf("zap: %w", err)
	}
	return &Harness{scenario: scenario, logger: logger}, nil
}

// Run drives the configured scenario to completion or until the OOM
// cap is exceeded. It blocks until the run finishes.
func (h *Harness) Run(ctx context.Context) (Summary, error) {
	startedAt := time.Now()
	runCtx, cancel := context.WithCancel(ctx)
	defer cancel()

	// Capture a baseline heap+goroutine profile before any workers
	// start. The operator diffs this against the end-of-run profile to
	// localise allocation sites.
	var profiles ProfilePaths
	if h.ProfileDir != "" {
		if err := os.MkdirAll(h.ProfileDir, 0o755); err != nil {
			return Summary{}, fmt.Errorf("mkdir profile dir: %w", err)
		}
		heapStart := filepath.Join(h.ProfileDir, "heap-start.pprof")
		grStart := filepath.Join(h.ProfileDir, "goroutine-start.pprof")
		if err := CaptureHeap(heapStart); err != nil {
			h.logger.Warn("failed to capture start heap profile", zap.Error(err))
		} else {
			profiles.HeapStart = "heap-start.pprof"
		}
		if err := CaptureGoroutine(grStart); err != nil {
			h.logger.Warn("failed to capture start goroutine profile", zap.Error(err))
		} else {
			profiles.GoroutineStart = "goroutine-start.pprof"
		}
	}

	// Deterministic baseline census (GC-settled) taken before any worker
	// starts. Diffed against the end-of-run census to detect leaks by
	// reachable-object/goroutine count rather than noisy RSS slope.
	startCounts := captureCounts()

	// Start fakes for each chain in the scenario using the family registry.
	fakes := make(map[string]common.FaultableServer)
	workers := make(map[string]common.Worker)
	for name, spec := range h.scenario.Chains {
		family, ok := familyRegistry[spec.Fake]
		if !ok {
			return Summary{}, fmt.Errorf("chain %q: no fake family registered for %q", name, spec.Fake)
		}
		fakes[name] = family.NewServer(stableChainID(name))
		workers[name] = family.NewWorker()
	}
	defer func() {
		for _, f := range fakes {
			f.Stop()
		}
	}()

	// Apply each chain's fault schedule on a separate goroutine.
	for chainName, spec := range h.scenario.Chains {
		spec := spec
		chainName := chainName
		go h.applyFaultSchedule(runCtx, fakes[chainName], spec.FaultSchedule, startedAt)
	}

	// Launch one worker per chain.
	var wg sync.WaitGroup
	for chainName := range h.scenario.Chains {
		wg.Add(1)
		fake := fakes[chainName]
		worker := workers[chainName]
		go func(_ string) {
			defer wg.Done()
			worker.Run(runCtx, fake.URL())
		}(chainName)
	}

	// Sample runtime stats on the configured interval.
	verdict := VerdictOK
	var samples []Sample
	var peakRSS uint64
	var peakGoroutines int

	deadline := time.NewTimer(h.scenario.Duration)
	defer deadline.Stop()
	sampleTick := time.NewTicker(h.scenario.SampleInterval)
	defer sampleTick.Stop()

	// Take an immediate first sample so duration < 2*sample_interval
	// scenarios still produce a regression input.
	samples = append(samples, captureSample())

samplingLoop:
	for {
		select {
		case <-ctx.Done():
			verdict = VerdictAborted
			break samplingLoop
		case <-deadline.C:
			break samplingLoop
		case <-sampleTick.C:
			s := captureSample()
			samples = append(samples, s)
			if s.RSSBytes > peakRSS {
				peakRSS = s.RSSBytes
			}
			if s.NumGoroutines > peakGoroutines {
				peakGoroutines = s.NumGoroutines
			}
			if h.scenario.OOMCapBytes > 0 && s.RSSBytes >= h.scenario.OOMCapBytes {
				h.logger.Warn("OOM cap exceeded; aborting scenario",
					zap.Uint64("rss_bytes", s.RSSBytes),
					zap.Uint64("cap_bytes", h.scenario.OOMCapBytes))
				verdict = VerdictKilledOOM
				break samplingLoop
			}
		}
	}

	cancel()
	// Bound the worker join so a misbehaving family that ignores
	// ctx.Done cannot deadlock the harness.
	joinDone := make(chan struct{})
	go func() {
		wg.Wait()
		close(joinDone)
	}()
	select {
	case <-joinDone:
	case <-time.After(30 * time.Second):
		h.logger.Warn("workers did not exit within 30s of cancel; continuing teardown")
	}

	// End-of-run profiles. Captured after workers exit so the snapshot
	// reflects steady-state allocations attributable to the workload,
	// not in-flight ones.
	if h.ProfileDir != "" {
		heapEnd := filepath.Join(h.ProfileDir, "heap-end.pprof")
		grEnd := filepath.Join(h.ProfileDir, "goroutine-end.pprof")
		if err := CaptureHeap(heapEnd); err != nil {
			h.logger.Warn("failed to capture end heap profile", zap.Error(err))
		} else {
			profiles.HeapEnd = "heap-end.pprof"
		}
		if err := CaptureGoroutine(grEnd); err != nil {
			h.logger.Warn("failed to capture end goroutine profile", zap.Error(err))
		} else {
			profiles.GoroutineEnd = "goroutine-end.pprof"
		}
	}

	// End-of-run census, taken after workers have exited so leaked
	// goroutines/objects are isolated from in-flight work.
	endCounts := captureCounts()
	counts := computeCountDeltas(startCounts, endCounts)
	growth := topGoroutineGrowth(startCounts, endCounts, 5)

	// Deterministic gate: if the scenario declares a goroutine-growth
	// ceiling and we exceeded it, flag the leak. Only downgrades a
	// healthy verdict — OOM/abort take precedence.
	if verdict == VerdictOK && h.scenario.MaxGoroutineGrowth > 0 &&
		counts.GoroutineDelta > h.scenario.MaxGoroutineGrowth {
		h.logger.Warn("goroutine growth exceeded scenario ceiling",
			zap.Int("delta", counts.GoroutineDelta),
			zap.Int("max", h.scenario.MaxGoroutineGrowth))
		verdict = VerdictLeak
	}

	summary := Summary{
		Scenario:           h.scenario.Name,
		Description:        h.scenario.Description,
		StartedAt:          startedAt,
		CompletedAt:        time.Now(),
		SampleCount:        len(samples),
		Verdict:            verdict,
		OOMCapBytes:        h.scenario.OOMCapBytes,
		PeakRSSBytes:       peakRSS,
		PeakGoroutines:     peakGoroutines,
		Counts:             counts,
		TopGoroutineGrowth: growth,
		Slopes:             ComputeSlopes(samples),
		Profiles:           profiles,
	}
	return summary, nil
}

func (h *Harness) applyFaultSchedule(ctx context.Context, fake common.FaultableServer, schedule []FaultEvent, runStart time.Time) {
	for _, ev := range schedule {
		wait := time.Until(runStart.Add(ev.At))
		if wait > 0 {
			select {
			case <-ctx.Done():
				return
			case <-time.After(wait):
			}
		}
		fake.Set(translateAction(ev.Action))
	}
}

func translateAction(a FaultAction) common.Fault {
	switch a {
	case FaultClose, FaultDisconnect:
		return common.FaultCloseAllConnections
	case FaultMalformed:
		return common.FaultMalformed
	case FaultSlow:
		return common.FaultSlow
	case FaultFreeze:
		return common.FaultStuck
	case FaultHeal:
		return common.FaultNone
	default:
		return common.FaultNone
	}
}

func captureSample() Sample {
	var ms runtime.MemStats
	runtime.ReadMemStats(&ms)
	rss := currentRSSBytes()
	return Sample{
		At:             time.Now(),
		RSSBytes:       rss,
		HeapInuseBytes: ms.HeapInuse,
		NumGoroutines:  runtime.NumGoroutine(),
		OpenFDs:        currentFDCount(),
	}
}

// currentRSSBytes reads /proc/self/status on Linux or falls back to
// HeapSys on other platforms. The fallback is imprecise but works for
// the macOS dev path.
func currentRSSBytes() uint64 {
	data, err := os.ReadFile("/proc/self/status")
	if err == nil {
		for _, line := range strings.Split(string(data), "\n") {
			if strings.HasPrefix(line, "VmRSS:") {
				var kb uint64
				if _, err := fmt.Sscanf(line, "VmRSS: %d kB", &kb); err == nil {
					return kb * 1024
				}
			}
		}
	}
	var ms runtime.MemStats
	runtime.ReadMemStats(&ms)
	return ms.Sys
}

// currentFDCount counts entries in /proc/self/fd on Linux, returns 0
// elsewhere. The slope is what matters, not the absolute count.
func currentFDCount() int {
	entries, err := os.ReadDir("/proc/self/fd")
	if err != nil {
		return 0
	}
	return len(entries)
}

// stableChainID hashes the chain name into a deterministic uint32-sized
// value. The collisions don't matter — each fake only needs to match
// against its own dial.
func stableChainID(name string) uint64 {
	h := fnv.New64a()
	_, _ = h.Write([]byte(name))
	return h.Sum64() & 0xffffffff
}

// WriteSummary dumps a Summary to a JSON file at `path`, creating parent
// dirs as needed.
func WriteSummary(path string, s Summary) error {
	if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
		return err
	}
	data, err := json.MarshalIndent(s, "", "  ")
	if err != nil {
		return err
	}
	return os.WriteFile(path, data, 0o644)
}

