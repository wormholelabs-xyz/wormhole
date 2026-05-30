package harness

import (
	"math"
	"testing"
	"time"

	"github.com/stretchr/testify/require"
)

func TestComputeSlopes_FewerThanTwoSamples(t *testing.T) {
	require.Equal(t, Slopes{}, ComputeSlopes(nil))
	require.Equal(t, Slopes{}, ComputeSlopes([]Sample{{}}))
}

func TestComputeSlopes_LinearGrowth(t *testing.T) {
	// 100 MiB/h RSS growth, exactly linear, over 1 hour at 1-minute steps.
	t0 := time.Now()
	const stepBytes = 100 * 1024 * 1024 / 60 // ~1.67 MiB per minute
	samples := make([]Sample, 0, 61)
	for i := 0; i <= 60; i++ {
		samples = append(samples, Sample{
			At:       t0.Add(time.Duration(i) * time.Minute),
			RSSBytes: uint64(i) * stepBytes,
		})
	}
	slopes := ComputeSlopes(samples)
	// stepBytes/min * 60min = ~100 MiB/h; allow a 1% tolerance for
	// integer truncation in stepBytes.
	require.InDelta(t, 100.0, slopes.RSSMBPerHour, 1.0, "slope should be ~100 MiB/h")
}

func TestComputeSlopes_ConstantSeries(t *testing.T) {
	t0 := time.Now()
	samples := []Sample{
		{At: t0, RSSBytes: 1_000_000_000},
		{At: t0.Add(time.Minute), RSSBytes: 1_000_000_000},
		{At: t0.Add(2 * time.Minute), RSSBytes: 1_000_000_000},
	}
	slopes := ComputeSlopes(samples)
	require.True(t, math.Abs(slopes.RSSMBPerHour) < 0.0001, "flat series must yield ~0 slope, got %f", slopes.RSSMBPerHour)
}
