package harness

import "time"

// Sample is one observation at a point in time.
type Sample struct {
	At              time.Time
	RSSBytes        uint64
	HeapInuseBytes  uint64
	NumGoroutines   int
	OpenFDs         int
}

// Slopes carries the per-hour rate of change for each tracked metric,
// derived by ordinary-least-squares linear regression of each metric
// against time.
type Slopes struct {
	RSSMBPerHour       float64
	HeapInuseMBPerHour float64
	GoroutinesPerHour  float64
	FDsPerHour         float64
}

// ComputeSlopes returns the per-hour slope for each metric. If fewer
// than two samples are present, every slope is zero (linear regression
// is undefined with one point).
func ComputeSlopes(samples []Sample) Slopes {
	if len(samples) < 2 {
		return Slopes{}
	}

	t0 := samples[0].At
	xs := make([]float64, len(samples))
	for i, s := range samples {
		xs[i] = s.At.Sub(t0).Hours()
	}

	const bytesPerMB = 1024.0 * 1024.0

	return Slopes{
		RSSMBPerHour:       fitSlope(xs, mapBytes(samples, func(s Sample) float64 { return float64(s.RSSBytes) / bytesPerMB })),
		HeapInuseMBPerHour: fitSlope(xs, mapBytes(samples, func(s Sample) float64 { return float64(s.HeapInuseBytes) / bytesPerMB })),
		GoroutinesPerHour:  fitSlope(xs, mapBytes(samples, func(s Sample) float64 { return float64(s.NumGoroutines) })),
		FDsPerHour:         fitSlope(xs, mapBytes(samples, func(s Sample) float64 { return float64(s.OpenFDs) })),
	}
}

func mapBytes(samples []Sample, f func(Sample) float64) []float64 {
	out := make([]float64, len(samples))
	for i, s := range samples {
		out[i] = f(s)
	}
	return out
}

// fitSlope returns the slope of an ordinary least-squares regression
// of ys on xs. xs and ys must have the same length and at least two
// distinct x values.
func fitSlope(xs, ys []float64) float64 {
	n := float64(len(xs))
	var sumX, sumY, sumXY, sumXX float64
	for i := range xs {
		sumX += xs[i]
		sumY += ys[i]
		sumXY += xs[i] * ys[i]
		sumXX += xs[i] * xs[i]
	}
	denom := n*sumXX - sumX*sumX
	if denom == 0 {
		return 0
	}
	return (n*sumXY - sumX*sumY) / denom
}
