#!/usr/bin/env bash
# Table-driven test of run_with_timeout_seconds (bounded_run.sh). Needs no Docker.
# Usage: canton/localnet/test/bounded_run_test.sh
set -uo pipefail
# shellcheck source=SCRIPTDIR/../bounded_run.sh
source "$(dirname "${BASH_SOURCE[0]}")/../bounded_run.sh"

failure_count=0
work_dir="$(mktemp -d)"
trap 'rm -rf "$work_dir"' EXIT

# Columns: case | timeout seconds | command | expected status | maximum elapsed seconds | stderr fragment ("-" for none)
# A command that starts a child writes its pid to $CHILD_PID_FILE; the stop must reach that child too.
readonly BOUNDED_RUN_CASES=(
  "finishes with success|5|true|0|2|-"
  "finishes with failure|5|sh -c 'exit 3'|3|2|-"
  "exceeds the timeout|1|sleep 30 & echo \$! > \"\$CHILD_PID_FILE\"; wait|124|5|stopped after 1s"
  "ignores TERM|1|trap '' TERM; sleep 30 & echo \$! > \"\$CHILD_PID_FILE\"; wait|124|15|stopped after 1s"
  "timeout zero|0|true|2|1|timeout must be 1 to 3600 seconds, got '0'"
  "timeout above the maximum|3601|true|2|1|timeout must be 1 to 3600 seconds, got '3601'"
  "timeout not a number|ten|true|2|1|timeout must be 1 to 3600 seconds, got 'ten'"
)

for bounded_run_case in "${BOUNDED_RUN_CASES[@]}"; do
  IFS='|' read -r case_name timeout_seconds command_text expected_status maximum_elapsed_seconds stderr_fragment \
    <<<"$bounded_run_case"
  marker="bounded-run-test-$$-$RANDOM"
  child_pid_file="$work_dir/child.pid"
  rm -f "$child_pid_file"
  started_at="$SECONDS"
  actual_status=0
  # The marker in argv lets the check below find a leftover process of this case.
  CHILD_PID_FILE="$child_pid_file" run_with_timeout_seconds "$timeout_seconds" bash -c "$command_text" "$marker" \
    2> "$work_dir/stderr" || actual_status=$?
  elapsed_seconds=$((SECONDS - started_at))
  case_failed=false
  if [[ "$actual_status" != "$expected_status" ]]; then
    echo "FAIL: $case_name: status $actual_status, expected $expected_status" >&2
    case_failed=true
  fi
  if ((elapsed_seconds > maximum_elapsed_seconds)); then
    echo "FAIL: $case_name: took ${elapsed_seconds}s, expected at most ${maximum_elapsed_seconds}s" >&2
    case_failed=true
  fi
  if [[ "$stderr_fragment" != "-" ]] && ! grep -qF -- "$stderr_fragment" "$work_dir/stderr"; then
    echo "FAIL: $case_name: stderr does not contain: $stderr_fragment" >&2
    cat "$work_dir/stderr" >&2
    case_failed=true
  fi
  if pgrep -f "$marker" >/dev/null; then
    echo "FAIL: $case_name: the command still runs" >&2
    pkill -KILL -f "$marker" || true
    case_failed=true
  fi
  if [[ -s "$child_pid_file" ]] && kill -0 "$(cat "$child_pid_file")" 2>/dev/null; then
    echo "FAIL: $case_name: a child of the command still runs" >&2
    kill -KILL "$(cat "$child_pid_file")" 2>/dev/null || true
    case_failed=true
  fi
  if [[ "$case_failed" == true ]]; then
    failure_count=$((failure_count + 1))
  else
    echo "PASS: $case_name (status $actual_status, ${elapsed_seconds}s)"
  fi
done

# Columns: case | signal sent to the caller
readonly CALLER_SIGNAL_CASES=("caller gets TERM|TERM" "caller gets INT|INT" "caller gets HUP|HUP")
bounded_run_library="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/bounded_run.sh"

for caller_signal_case in "${CALLER_SIGNAL_CASES[@]}"; do
  IFS='|' read -r case_name caller_signal <<<"$caller_signal_case"
  marker="bounded-run-signal-test-$$-$RANDOM"
  child_pid_file="$work_dir/child.pid"
  rm -f "$child_pid_file"
  # Job control keeps INT deliverable: a non-interactive shell ignores INT in plain background jobs.
  set -m
  CHILD_PID_FILE="$child_pid_file" bash -c 'source "$1"; run_with_timeout_seconds 600 bash -c "sleep 300 & echo \$! > \"\$CHILD_PID_FILE\"; wait" "$2"' \
    caller "$bounded_run_library" "$marker" 2>/dev/null &
  caller_pid=$!
  set +m
  for _ in 1 2 3 4 5 6 7 8 9 10; do
    [[ -s "$child_pid_file" ]] && break
    sleep 0.5
  done
  kill "-$caller_signal" "$caller_pid" 2>/dev/null || true
  wait "$caller_pid" 2>/dev/null || true
  sleep 1
  case_failed=false
  if pgrep -f "$marker" >/dev/null; then
    echo "FAIL: $case_name: the command still runs" >&2
    pkill -KILL -f "$marker" || true
    case_failed=true
  fi
  if [[ ! -s "$child_pid_file" ]]; then
    echo "FAIL: $case_name: the command did not start" >&2
    case_failed=true
  elif kill -0 "$(cat "$child_pid_file")" 2>/dev/null; then
    echo "FAIL: $case_name: a child of the command still runs" >&2
    kill -KILL "$(cat "$child_pid_file")" 2>/dev/null || true
    case_failed=true
  fi
  if [[ "$case_failed" == true ]]; then
    failure_count=$((failure_count + 1))
  else
    echo "PASS: $case_name"
  fi
done

if ((failure_count > 0)); then
  echo "$failure_count case(s) failed" >&2
  exit 1
fi
echo "All $((${#BOUNDED_RUN_CASES[@]} + ${#CALLER_SIGNAL_CASES[@]})) cases passed"
