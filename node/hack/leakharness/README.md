# leakharness

In-process harness for reproducing and regression-testing guardian
watcher memory/goroutine leaks. It drives chain-watcher connector
lifecycles against controllable fake RPC servers and reports a
deterministic, count-based leak signal per scenario.

This is a diagnostic tool maintained on a private branch. It is not part
of the guardian binary and is intentionally not upstreamed; the
production fixes it was built to validate ship separately.

## How leaks are detected: counts, not slopes

The harness reports two kinds of signal. Read them in this order:

1. **Counts (primary, deterministic).** At scenario start and end the
   harness forces a GC and records reachable `HeapObjects`, `HeapAlloc`,
   and the live goroutine count. The start→end **deltas** are the leak
   signal: a leak shows as a large positive `goroutine_delta` and/or
   `heap_objects_delta`. Because the GC settles first, these measure the
   program's own reachable state, not OS noise.

2. **Slopes (report-only).** Per-hour linear-regression slopes of RSS,
   heap-inuse, goroutines, and FDs across the sampled run. **Do not gate
   on these.** RSS in particular is OS-level: Go returns memory to the OS
   lazily (the scavenger), and GC sawtooth plus process warmup dominate
   short windows. Empirically, repeated matched runs showed the RSS slope
   stddev (~25-30 MB/h) swamping any leak signal — the "leaky" and
   "healthy" runs were statistically indistinguishable by RSS slope. The
   slopes are kept only for trend context.

`top_goroutine_growth` names the goroutine stacks that grew the most over
the run — for an unclosed connector it points straight at the leaked
`rpc.(*Client).dispatch` / `pingLoop` / socket-read goroutines.

### The gate

Set `max_goroutine_growth: <n>` on a scenario to turn it into a hard
gate: if the GC-settled `goroutine_delta` exceeds `n`, the verdict is
`leak_detected` and the CLI exits non-zero. Without it, the run is
report-only (`ok` unless the OOM cap is hit).

## Layout

```
cmd/leakharness/      CLI entry point (run subcommand)
harness/              orchestrator: scenario load, census, sampling, slopes, OOM cap, pprof
  counts.go           GC-settled goroutine/heap census + stack-diff (the primary signal)
fakerpc/common/       chain-family abstraction and fault registry
fakerpc/{evm,sui,cosmwasm,xrpl}/   per-family fake RPC servers + workers
scenarios/            scenario definitions (*.yaml)
scripts/nightly.sh    batch runner over every scenario
```

## Build & run

```sh
cd node
go build -o /tmp/leakharness ./hack/leakharness/cmd/leakharness
# NOTE: flags must precede the scenario path (Go flag parsing stops at the
# first positional argument).
/tmp/leakharness run --out runs/leak hack/leakharness/scenarios/connector_leak_selftest.yaml
```

Flags: `--out <dir>` (default `runs/<scenario>-<ts>/`); `--pprof-addr <host:port>`
(live pprof, default `127.0.0.1:6061`, loopback-only because heap dumps
can contain sensitive state; empty string disables).

## Scenarios

- `connector_leak_selftest` — **negative control / self-test.** Uses the
  `evm_leak` family, whose worker dials connectors and abandons them
  WITHOUT Close (reproducing the pre-fix supervisor restart). It is
  EXPECTED to end `leak_detected` with a large `goroutine_delta` naming
  the `rpc.Client` stacks. If this ever passes, the harness has stopped
  detecting the leak class it exists for.
- `steady_state` — healthy baseline; the closing `evm` worker, no faults.
- `mezo_flap` / `multi_chain_flap` — flapping scenarios. Note: a
  `close_websockets` flap drops the connection, which lets a leaked
  connector's goroutines self-terminate on EOF — so flap scenarios do
  NOT reproduce the goroutine leak. Use `connector_leak_selftest` (live
  connection, no Close) for that.

`_defaults.yaml` holds shared defaults inherited via each scenario's
`defaults:` key.

## EVM families: `evm` vs `evm_leak`

- `evm` — the worker dials, holds, and **Closes** each connector. Models
  the fixed behaviour; goroutine delta stays flat.
- `evm_leak` — the worker dials and **never Closes** (bounded to protect
  the FD limit). Models the bug; goroutine delta climbs ~3 per dial.

## Localising "why is memory growing" — tooling guide

The harness output names the growing goroutine stack directly. For deeper
analysis, the right tool depends on the leak shape (this distinction
matters a lot):

- **Goroutine-held leak** (a goroutine that never exits keeps its objects
  alive — our connector case): the goroutine profile is the instrument.
  `top_goroutine_growth` already names it; for full stacks diff the
  captured profiles:
  ```sh
  go tool pprof -base goroutine-start.pprof goroutine-end.pprof
  ```
  In tests, `uber-go/goleak` or `runtime.NumGoroutine()` deltas assert it
  deterministically.

- **Reachable-growth leak** (e.g. a map that grows and is never trimmed):
  a heap profile shows WHERE objects were allocated, but NOT what RETAINS
  them. To find the retaining reference chain use a heap-reference
  analyzer — `goref` (github.com/cloudwego/goref) is the current,
  maintained tool (Delve-based; `grf attach <pid>` or `grf core`, then
  `go tool pprof grf.out`). `viewcore`/`gocore` is unmaintained and broken
  on modern Go. The heap-site half is:
  ```sh
  go tool pprof -base heap-start.pprof heap-end.pprof   # -inuse_space / -inuse_objects
  ```

- **Confirm vs fragmentation:** `runtime.ReadMemStats` `HeapAlloc`/
  `HeapObjects` rising across GC = real reachable growth; a small
  `HeapInuse - HeapAlloc` rules out fragmentation. `GODEBUG=gctrace=1`
  prints live-heap-after-GC per cycle for the same purpose.

- **Not the tool here:** Go 1.25's valgrind support targets invalid/
  unreachable/native (cgo) memory. It does NOT see reachable Go growth or
  goroutine leaks (a parked goroutine and a growing map are both valid,
  reachable memory). Reach for it only for cgo/native leaks.

## Nightly batch

`scripts/nightly.sh` builds the binary once, runs every scenario, and
prints a slope table. Requires `jq`. `connector_leak_selftest` is
expected to report `leak_detected` there — that is the harness proving it
still works. Override the output root with `RUN_ROOT=/tmp/...`.

## Tests

```sh
cd node
go test ./hack/leakharness/...
```
