#!/usr/bin/env bash
#
# nightly.sh — run every shipped scenario back-to-back and emit a
# tabular slope summary. Suitable for cron on a dev box; not for CI.
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

# Tabular header.
printf '%-20s %-14s %14s %18s %16s %18s\n' \
  "scenario" "verdict" "rss_mb_per_h" "heap_inuse_mb_p_h" "goroutines_p_h" "peak_rss_MiB"
printf '%-20s %-14s %14s %18s %16s %18s\n' \
  "--------" "-------" "------------" "-----------------" "--------------" "------------"

for scenario in "${SCENARIOS[@]}"; do
  name="$(basename "$scenario" .yaml)"
  outdir="$BATCH_DIR/$name"
  mkdir -p "$outdir"
  log="$outdir/console.log"

  if ! "$BIN" run --out "$outdir" "$scenario" >"$log" 2>&1; then
    printf '%-20s %-14s %14s %18s %16s %18s\n' "$name" "ERROR" "-" "-" "-" "-"
    continue
  fi

  summary="$outdir/summary.json"
  if [ ! -f "$summary" ]; then
    printf '%-20s %-14s %14s %18s %16s %18s\n' "$name" "NO_SUMMARY" "-" "-" "-" "-"
    continue
  fi

  verdict="$(jq -r '.verdict' "$summary")"
  rss_slope="$(jq -r '.slopes.rss_mb_per_hour' "$summary")"
  heap_slope="$(jq -r '.slopes.heap_inuse_mb_per_hour' "$summary")"
  gr_slope="$(jq -r '.slopes.goroutines_per_hour' "$summary")"
  peak_rss="$(jq -r '.peak_rss_bytes / 1024 / 1024 | floor' "$summary")"

  printf '%-20s %-14s %14.2f %18.2f %16.2f %18d\n' \
    "$name" "$verdict" "$rss_slope" "$heap_slope" "$gr_slope" "$peak_rss"
done

echo
echo "done. summaries under $BATCH_DIR"
