#!/usr/bin/env bash
# Table-driven test of canary_gate_decision (watcher_harness_lib.sh). Needs bash.
# Usage: canton/localnet/test/canary_gate_decision_test.sh
set -uo pipefail
LOCALNET_TEST_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
readonly LOCALNET_TEST_DIR
# shellcheck source=SCRIPTDIR/../watcher_harness_lib.sh
source "$LOCALNET_TEST_DIR/../watcher_harness_lib.sh"

# <case name>|<now> <gate start> <window start> <quiet window> <maximum>|<status>|<output or stderr fragment>
readonly DECISION_CASES=(
  "window complete at its end|20 0 0 20 120|0|passed"
  "one second before the window ends|19 0 0 20 120|0|waiting"
  "restarted window, one second short|119 0 100 20 120|0|waiting"
  "restarted window ends exactly at the maximum|120 0 100 20 120|0|passed"
  "restarted window ends after the maximum|120 0 101 20 120|0|timed-out"
  "after the maximum|121 0 101 20 120|0|timed-out"
  "restarted window cannot end in time, maximum not reached|110 0 101 20 120|0|waiting"
  "72 h maximum, window complete|259200 0 255600 3600 259200|0|passed"
  "gate start after zero|50 30 30 20 120|0|passed"
  "quiet window zero|0 0 0 0 120|2|quiet window"
  "maximum below the quiet window|10 0 0 20 10|2|below the quiet window"
  "window start before gate start|20 10 0 20 120|2|before the gate start"
  "now before window start|10 0 20 20 120|2|before the window start"
  "not an integer|a 0 0 20 120|2|not a non-negative integer"
  "negative number|-1 0 0 20 120|2|not a non-negative integer"
  "leading zero|08 0 0 20 120|2|not a non-negative integer"
  "four arguments|20 0 0 20|2|expected 5 arguments"
)

failure_count=0
stderr_file="$(mktemp)"
trap 'rm -f "$stderr_file"' EXIT

for decision_case in "${DECISION_CASES[@]}"; do
  IFS='|' read -r case_name case_arguments expected_status expected_text <<<"$decision_case"
  actual_status=0
  # shellcheck disable=SC2086 # the arguments are space-separated numbers
  actual_output="$(canary_gate_decision $case_arguments 2> "$stderr_file")" || actual_status=$?
  if [[ "$actual_status" != "$expected_status" ]]; then
    echo "FAIL: $case_name: status $actual_status, expected $expected_status ($(cat "$stderr_file"))" >&2
    failure_count=$((failure_count + 1))
  elif [[ "$expected_status" == 0 && "$actual_output" != "$expected_text" ]]; then
    echo "FAIL: $case_name: output '$actual_output', expected '$expected_text'" >&2
    failure_count=$((failure_count + 1))
  elif [[ "$expected_status" != 0 ]] && ! grep -qF -- "$expected_text" "$stderr_file"; then
    echo "FAIL: $case_name: stderr '$(cat "$stderr_file")' lacks '$expected_text'" >&2
    failure_count=$((failure_count + 1))
  elif [[ "$expected_status" != 0 && -n "$actual_output" ]]; then
    echo "FAIL: $case_name: printed '$actual_output' on an error" >&2
    failure_count=$((failure_count + 1))
  else
    echo "ok: $case_name"
  fi
done

if [[ "$failure_count" -ne 0 ]]; then
  echo "canary_gate_decision_test.sh: $failure_count failures" >&2
  exit 1
fi
echo "canary_gate_decision_test.sh: PASS"
