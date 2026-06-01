// Command leakharness drives a guardian leak-detection scenario and
// writes a JSON summary plus a sample time series to disk.
//
// Usage:
//
//	leakharness run <scenario.yaml> [--out runs/<ts>/]
package main

import (
	"context"
	"flag"
	"fmt"
	"os"
	"path/filepath"
	"time"

	"github.com/certusone/wormhole/node/hack/leakharness/harness"
)

func main() {
	if len(os.Args) < 2 {
		fmt.Fprintln(os.Stderr, "usage: leakharness run <scenario.yaml> [--out <dir>]")
		os.Exit(2)
	}
	switch os.Args[1] {
	case "run":
		exitCode := runCmd(os.Args[2:])
		os.Exit(exitCode)
	default:
		fmt.Fprintf(os.Stderr, "unknown subcommand %q\n", os.Args[1])
		os.Exit(2)
	}
}

func runCmd(args []string) int {
	fs := flag.NewFlagSet("run", flag.ExitOnError)
	out := fs.String("out", "", "output directory (default: runs/<scenario>-<ts>/)")
	pprofAddr := fs.String("pprof-addr", "127.0.0.1:6061",
		"address for live net/http/pprof server (set to empty string to disable; bind only to loopback because heap dumps may contain sensitive state)")
	fs.Usage = func() {
		fmt.Fprintln(os.Stderr, "usage: leakharness run [--out <dir>] [--pprof-addr <host:port>] <scenario.yaml>")
		fs.PrintDefaults()
	}
	if err := fs.Parse(args); err != nil {
		return 2
	}
	if fs.NArg() < 1 {
		fs.Usage()
		return 2
	}
	scenarioPath := fs.Arg(0)

	scenario, err := harness.LoadScenario(scenarioPath)
	if err != nil {
		fmt.Fprintln(os.Stderr, "load scenario:", err)
		return 1
	}

	outDir := *out
	if outDir == "" {
		ts := time.Now().UTC().Format("20060102T150405Z")
		outDir = filepath.Join("runs", fmt.Sprintf("%s-%s", scenario.Name, ts))
	}
	if err := os.MkdirAll(outDir, 0o755); err != nil {
		fmt.Fprintln(os.Stderr, "mkdir:", err)
		return 1
	}

	h, err := harness.New(scenario)
	if err != nil {
		fmt.Fprintln(os.Stderr, "new harness:", err)
		return 1
	}
	h.ProfileDir = outDir

	stopPprof, pprofAddrActual, err := harness.StartPprofServer(*pprofAddr)
	if err != nil {
		fmt.Fprintln(os.Stderr, "pprof server:", err)
		return 1
	}
	defer func() { _ = stopPprof() }()

	ctx, cancel := context.WithTimeout(context.Background(), scenario.Duration+2*time.Minute)
	defer cancel()

	fmt.Printf("running scenario %q for %s, sample interval %s, OOM cap %d MiB\n",
		scenario.Name, scenario.Duration, scenario.SampleInterval, scenario.OOMCapBytes>>20)
	if pprofAddrActual != "" {
		fmt.Printf("live pprof: http://%s/debug/pprof/ (heap, goroutine, profile, allocs)\n", pprofAddrActual)
	}

	summary, err := h.Run(ctx)
	if err != nil {
		fmt.Fprintln(os.Stderr, "run:", err)
		return 1
	}

	summaryPath := filepath.Join(outDir, "summary.json")
	if err := harness.WriteSummary(summaryPath, summary); err != nil {
		fmt.Fprintln(os.Stderr, "write summary:", err)
		return 1
	}

	fmt.Printf("\nverdict: %s\n", summary.Verdict)
	fmt.Printf("samples: %d\n", summary.SampleCount)
	fmt.Printf("peak RSS: %d MiB\n", summary.PeakRSSBytes>>20)
	fmt.Printf("peak goroutines: %d\n", summary.PeakGoroutines)
	fmt.Printf("slopes:\n")
	fmt.Printf("  rss_mb_per_hour:        %.2f\n", summary.Slopes.RSSMBPerHour)
	fmt.Printf("  heap_inuse_mb_per_hour: %.2f\n", summary.Slopes.HeapInuseMBPerHour)
	fmt.Printf("  goroutines_per_hour:    %.2f\n", summary.Slopes.GoroutinesPerHour)
	fmt.Printf("  fds_per_hour:           %.2f\n", summary.Slopes.FDsPerHour)
	fmt.Printf("\nsummary written to %s\n", summaryPath)
	if summary.Profiles.HeapStart != "" && summary.Profiles.HeapEnd != "" {
		fmt.Printf("heap diff:    go tool pprof -base %s %s\n",
			filepath.Join(outDir, summary.Profiles.HeapStart),
			filepath.Join(outDir, summary.Profiles.HeapEnd))
	}
	if summary.Profiles.GoroutineEnd != "" {
		fmt.Printf("goroutines:   go tool pprof %s\n",
			filepath.Join(outDir, summary.Profiles.GoroutineEnd))
	}

	if summary.Verdict == harness.VerdictKilledOOM {
		return 1
	}
	return 0
}

