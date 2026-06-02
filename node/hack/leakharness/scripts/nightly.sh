#!/usr/bin/env bash
#
# nightly.sh — run every shipped scenario back-to-back and emit a
# count-based leak summary. The table leads with the deterministic signal
# (verdict + GC-settled goroutine/heap-object deltas); the RSS slope is
# shown last and is report-only (noisy — do not gate on it). Suitable for
# cron on a dev box; not for CI.
#
# Output layout:
#   $RUN_ROOT/<scenario>-<ts>/summary.json
#   $RUN_ROOT/<scenario>-<ts>/console.log
#
# Defaults RUN_ROOT to
#   $HOME/WormholeLabs/CoreTeam/ClaudeInvestigations/guardian-investigations/harness/runs
# but can be overridden via env.

set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
HARNESS_DIR="$(cd "$HERE/.." && pwd)"
SCENARIO_DIR="$HARNESS_DIR/scenarios"
RUN_ROOT="${RUN_ROOT:-$HOME/WormholeLabs/CoreTeam/ClaudeInvestigations/guardian-investigations/harness/runs}"

mkdir -p "$RUN_ROOT"
TS="$(date -u +%Y%m%dT%H%M%SZ)"
BATCH_DIR="$RUN_ROOT/$TS"
mkdir -p "$BATCH_DIR"

echo "leakharness nightly run @ $TS"
echo "writing to $BATCH_DIR"
echo

# Build the binary once for the whole batch.
BIN="$BATCH_DIR/leakharness"
(cd "$HARNESS_DIR/../.." && go build -o "$BIN" ./hack/leakharness/cmd/leakharness)
echo "binary: $BIN"
echo

# Collect every scenario yaml except the defaults stub.
mapfile -t SCENARIOS < <(find "$SCENARIO_DIR" -maxdepth 1 -name '*.yaml' \! -name '_*.yaml' | sort)

# Tabular header. Lead with the deterministic count-based signal; the
# rss_mb/h slope trails and is report-only (noisy).
printf '%-24s %-14s %10s %14s %12s %10s\n' \
  "scenario" "verdict" "gr_delta" "heap_obj_d" "rss_mb/h*" "peak_MiB"
printf '%-24s %-14s %10s %14s %12s %10s\n' \
  "--------" "-------" "--------" "----------" "---------" "--------"

for scenario in "${SCENARIOS[@]}"; do
  name="$(basename "$scenario" .yaml)"
  outdir="$BATCH_DIR/$name"
  mkdir -p "$outdir"
  log="$outdir/console.log"

  # The CLI exits non-zero on leak_detected / killed_oom; capture that
  # without tripping `set -e` so the table still prints every row.
  if ! "$BIN" run --out "$outdir" "$scenario" >"$log" 2>&1; then
    : # non-zero is expected for leak_detected; the verdict column carries the truth
  fi

  summary="$outdir/summary.json"
  if [ ! -f "$summary" ]; then
    printf '%-24s %-14s %10s %14s %12s %10s\n' "$name" "NO_SUMMARY" "-" "-" "-" "-"
    continue
  fi

  verdict="$(jq -r '.verdict' "$summary")"
  gr_delta="$(jq -r '.counts.goroutine_delta' "$summary")"
  heap_obj_delta="$(jq -r '.counts.heap_objects_delta' "$summary")"
  rss_slope="$(jq -r '.slopes.rss_mb_per_hour' "$summary")"
  peak_rss="$(jq -r '.peak_rss_bytes / 1024 / 1024 | floor' "$summary")"

  printf '%-24s %-14s %10d %14d %12.2f %10d\n' \
    "$name" "$verdict" "$gr_delta" "$heap_obj_delta" "$rss_slope" "$peak_rss"
done

echo
echo "* rss_mb/h is report-only (noisy); gate on verdict and gr_delta."
echo "done. summaries under $BATCH_DIR"
