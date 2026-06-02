package harness

import (
	"runtime"
	"sort"
	"strings"
)

// CountSnapshot is a deterministic, GC-settled census of the live heap
// and goroutines at a single instant. Unlike the per-hour Slopes (which
// regress noisy RSS samples and cannot reliably separate a leak from
// runtime/scavenger jitter), these counts measure the program's own
// reachable state: a growing HeapObjects or goroutine count across a
// run is a leak, full stop.
type CountSnapshot struct {
	Goroutines  int
	HeapObjects uint64
	HeapAlloc   uint64
	// hist maps a goroutine stack signature to the number of goroutines
	// parked on it. Diffing two histograms names the leaking stack.
	hist map[string]int
}

// captureCounts forces a GC so only reachable objects are counted, then
// snapshots heap and goroutine state. The GC is the load-bearing step:
// without it HeapObjects/HeapAlloc include unswept garbage and the delta
// is meaningless.
func captureCounts() CountSnapshot {
	runtime.GC()
	var ms runtime.MemStats
	runtime.ReadMemStats(&ms)
	return CountSnapshot{
		Goroutines:  runtime.NumGoroutine(),
		HeapObjects: ms.HeapObjects,
		HeapAlloc:   ms.HeapAlloc,
		hist:        goroutineHistogram(),
	}
}

// goroutineHistogram groups every live goroutine by its stack signature
// and counts the occurrences. It uses the structured runtime.GoroutineProfile
// API rather than parsing pprof text.
func goroutineHistogram() map[string]int {
	n, _ := runtime.GoroutineProfile(nil)
	var recs []runtime.StackRecord
	for {
		recs = make([]runtime.StackRecord, n+16)
		m, ok := runtime.GoroutineProfile(recs)
		if ok {
			recs = recs[:m]
			break
		}
		n = m
	}
	hist := make(map[string]int, len(recs))
	for i := range recs {
		hist[stackLabel(recs[i].Stack())]++
	}
	return hist
}

// stackLabel renders a goroutine's stack into a compact, stable signature
// of up to six frames, leaf first, with the module path trimmed so the
// label reads e.g. "rpc.(*Client).dispatch < rpc.(*Client).read".
func stackLabel(pcs []uintptr) string {
	if len(pcs) == 0 {
		return "(no stack)"
	}
	frames := runtime.CallersFrames(pcs)
	var names []string
	for {
		f, more := frames.Next()
		name := f.Function
		if name == "" {
			name = "(unknown)"
		} else if i := strings.LastIndex(name, "/"); i >= 0 {
			name = name[i+1:]
		}
		names = append(names, name)
		if !more || len(names) >= 6 {
			break
		}
	}
	return strings.Join(names, " < ")
}

// CountDeltas is the JSON-serialisable start-vs-end census. A leak shows
// here as a large positive GoroutineDelta and/or HeapObjectsDelta.
type CountDeltas struct {
	GoroutinesStart  int    `json:"goroutines_start"`
	GoroutinesEnd    int    `json:"goroutines_end"`
	GoroutineDelta   int    `json:"goroutine_delta"`
	HeapObjectsStart uint64 `json:"heap_objects_start"`
	HeapObjectsEnd   uint64 `json:"heap_objects_end"`
	HeapObjectsDelta int64  `json:"heap_objects_delta"`
	HeapAllocStart   uint64 `json:"heap_alloc_start_bytes"`
	HeapAllocEnd     uint64 `json:"heap_alloc_end_bytes"`
	HeapAllocDelta   int64  `json:"heap_alloc_delta_bytes"`
}

// StackGrowth records one goroutine stack whose count rose over the run.
type StackGrowth struct {
	Stack      string `json:"stack"`
	StartCount int    `json:"start_count"`
	EndCount   int    `json:"end_count"`
	Delta      int    `json:"delta"`
}

func computeCountDeltas(start, end CountSnapshot) CountDeltas {
	return CountDeltas{
		GoroutinesStart:  start.Goroutines,
		GoroutinesEnd:    end.Goroutines,
		GoroutineDelta:   end.Goroutines - start.Goroutines,
		HeapObjectsStart: start.HeapObjects,
		HeapObjectsEnd:   end.HeapObjects,
		HeapObjectsDelta: int64(end.HeapObjects) - int64(start.HeapObjects),
		HeapAllocStart:   start.HeapAlloc,
		HeapAllocEnd:     end.HeapAlloc,
		HeapAllocDelta:   int64(end.HeapAlloc) - int64(start.HeapAlloc),
	}
}

// topGoroutineGrowth returns the stacks whose goroutine count rose the
// most between the two snapshots, capped at k. This is the "name the
// culprit" output: for an unclosed connector it surfaces the leaked
// rpc.Client dispatch/read/write stacks directly.
func topGoroutineGrowth(start, end CountSnapshot, k int) []StackGrowth {
	seen := make(map[string]bool)
	for s := range start.hist {
		seen[s] = true
	}
	for s := range end.hist {
		seen[s] = true
	}
	var out []StackGrowth
	for s := range seen {
		d := end.hist[s] - start.hist[s]
		if d > 0 {
			out = append(out, StackGrowth{Stack: s, StartCount: start.hist[s], EndCount: end.hist[s], Delta: d})
		}
	}
	sort.Slice(out, func(i, j int) bool {
		if out[i].Delta != out[j].Delta {
			return out[i].Delta > out[j].Delta
		}
		return out[i].Stack < out[j].Stack
	})
	if len(out) > k {
		out = out[:k]
	}
	return out
}
