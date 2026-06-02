package harness

import "testing"

func TestComputeCountDeltas_ReportsSignedDeltas(t *testing.T) {
	start := CountSnapshot{Goroutines: 10, HeapObjects: 1000, HeapAlloc: 5 << 20}
	end := CountSnapshot{Goroutines: 250, HeapObjects: 900, HeapAlloc: 9 << 20}

	d := computeCountDeltas(start, end)

	if d.GoroutineDelta != 240 {
		t.Errorf("GoroutineDelta = %d, want 240", d.GoroutineDelta)
	}
	// HeapObjects shrank; delta must be negative (int64, not underflowed uint).
	if d.HeapObjectsDelta != -100 {
		t.Errorf("HeapObjectsDelta = %d, want -100", d.HeapObjectsDelta)
	}
	if d.HeapAllocDelta != 4<<20 {
		t.Errorf("HeapAllocDelta = %d, want %d", d.HeapAllocDelta, 4<<20)
	}
	if d.GoroutinesStart != 10 || d.GoroutinesEnd != 250 {
		t.Errorf("start/end goroutines = %d/%d, want 10/250", d.GoroutinesStart, d.GoroutinesEnd)
	}
}

func TestTopGoroutineGrowth_RanksByDeltaAndIgnoresNonGrowth(t *testing.T) {
	start := CountSnapshot{hist: map[string]int{
		"rpc.(*Client).dispatch": 1,
		"steady.stack":           4,
		"shrinking.stack":        9,
	}}
	end := CountSnapshot{hist: map[string]int{
		"rpc.(*Client).dispatch": 61, // +60  (biggest)
		"poll.(*FD).Read":        20, // +20  (new stack)
		"steady.stack":           4,  //  0   (excluded)
		"shrinking.stack":        2,  // -7   (excluded)
	}}

	got := topGoroutineGrowth(start, end, 5)

	if len(got) != 2 {
		t.Fatalf("got %d growing stacks, want 2 (non-growing must be excluded): %+v", len(got), got)
	}
	if got[0].Stack != "rpc.(*Client).dispatch" || got[0].Delta != 60 {
		t.Errorf("top stack = %q delta %d, want rpc.(*Client).dispatch delta 60", got[0].Stack, got[0].Delta)
	}
	if got[1].Stack != "poll.(*FD).Read" || got[1].Delta != 20 {
		t.Errorf("second stack = %q delta %d, want poll.(*FD).Read delta 20", got[1].Stack, got[1].Delta)
	}
	if got[0].StartCount != 1 || got[0].EndCount != 61 {
		t.Errorf("top stack start/end = %d/%d, want 1/61", got[0].StartCount, got[0].EndCount)
	}
}

func TestTopGoroutineGrowth_HonoursCap(t *testing.T) {
	start := CountSnapshot{hist: map[string]int{}}
	end := CountSnapshot{hist: map[string]int{"a": 5, "b": 4, "c": 3, "d": 2}}

	got := topGoroutineGrowth(start, end, 2)

	if len(got) != 2 {
		t.Fatalf("got %d, want 2 (capped)", len(got))
	}
	if got[0].Stack != "a" || got[1].Stack != "b" {
		t.Errorf("got %q,%q, want a,b (highest deltas)", got[0].Stack, got[1].Stack)
	}
}
