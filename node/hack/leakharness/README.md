# leakharness

In-process harness for reproducing and regression-testing guardian
watcher memory/goroutine leaks. It drives chain-watcher connector
lifecycles against controllable fake RPC servers and reports
memory/goroutine/FD slope per scenario.

This is a diagnostic tool maintained on a private branch. It is not part
of the guardian binary and is intentionally not upstreamed; the
production fixes it was built to validate ship separately.

## Layout

```
cmd/leakharness/      CLI entry point (run subcommand)
harness/              orchestrator: scenario load, sampling, slope regression, OOM cap, pprof
fakerpc/common/       chain-family abstraction and fault registry
fakerpc/{evm,sui,cosmwasm,xrpl}/   per-family fake RPC servers
scenarios/            scenario definitions (*.yaml)
scripts/nightly.sh    batch runner over every scenario
```

## Build

```sh
cd node
go build -o /tmp/leakharness ./hack/leakharness/cmd/leakharness
```

## Run

```sh
/tmp/leakharness run hack/leakharness/scenarios/mezo_flap.yaml --out runs/mezo
```

Flags:

- `--out <dir>` — output directory (default `runs/<scenario>-<ts>/`).
- `--pprof-addr <host:port>` — live `net/http/pprof` server, default
  `127.0.0.1:6061`. Bound to loopback only because heap dumps can
  contain sensitive state (signing keys, RPC tokens); set to an empty
  string to disable.

The process self-terminates if RSS exceeds the scenario's OOM cap
(default 6 GiB, matching the production `GOMEMLIMIT`). Slopes are
report-only and do not fail the run.

## Scenarios

- `steady_state` — no faults; baseline slope under normal operation.
- `mezo_flap` — single EVM chain drops all WebSocket clients every 30s
  and heals 1s later, forcing a connector restart each cycle. This is
  the regression-detection scenario for the EVM connector leak.
- `multi_chain_flap` — several chains flapping concurrently.

`_defaults.yaml` holds shared defaults (sample interval, OOM cap)
inherited via each scenario's `defaults:` key.

## Output and profiling

Each run writes to the output directory:

- `summary.json` — verdict, sample count, peak RSS/goroutines, and
  per-hour slopes (`rss_mb_per_hour`, `heap_inuse_mb_per_hour`,
  `goroutines_per_hour`, `fds_per_hour`).
- `heap-start.pprof` / `heap-end.pprof` and
  `goroutine-start.pprof` / `goroutine-end.pprof`.

Localise an allocation site:

```sh
go tool pprof -base runs/mezo/heap-start.pprof runs/mezo/heap-end.pprof
go tool pprof runs/mezo/goroutine-end.pprof
```

## Nightly batch

`scripts/nightly.sh` builds the binary once, runs every scenario in
series, and prints a tabular slope summary. Requires `jq`. The output
root defaults to a local path and is overridable:

```sh
RUN_ROOT=/tmp/leakharness-runs ./hack/leakharness/scripts/nightly.sh
```

## Chain families

The EVM family wires the real `EthereumBaseConnector`, so the
connector-restart leak class is exercised directly. The Sui, Cosmwasm,
and XRPL families are generic HTTP/WebSocket lifecycle fakes.

## Tests

```sh
cd node
go test ./hack/leakharness/...
```
