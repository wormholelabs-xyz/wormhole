# run_with_timeout_seconds for the LocalNet scripts. Source this file; do not run it.
# macOS has no `timeout` command by default, so a watchdog process enforces the deadline.
# shellcheck shell=bash

readonly BOUNDED_RUN_MAX_SECONDS=3600
readonly BOUNDED_RUN_TIMEOUT_STATUS=124
readonly BOUNDED_RUN_KILL_GRACE_SECONDS=10

# run_with_timeout_seconds <seconds> <command...>: the command's exit status, or 124 after a stop at the deadline.
# The command runs in its own process group. The stop sends TERM to that group, then KILL after
# BOUNDED_RUN_KILL_GRACE_SECONDS, so children of the command stop too.
run_with_timeout_seconds() {
  local timeout_seconds="$1" command_pid watchdog_pid deadline_reached_file command_status=0 job_control_was_on=false
  shift
  if [[ ! "$timeout_seconds" =~ ^[1-9][0-9]*$ ]] || ((timeout_seconds > BOUNDED_RUN_MAX_SECONDS)); then
    echo "timeout must be 1 to $BOUNDED_RUN_MAX_SECONDS seconds, got '$timeout_seconds'" >&2
    return 2
  fi
  if (($# == 0)); then
    echo "run_with_timeout_seconds: no command" >&2
    return 2
  fi
  deadline_reached_file="$(mktemp)"
  [[ "$-" == *m* ]] && job_control_was_on=true
  # Job control gives each background job its own process group, with the job pid as group id.
  set -m
  "$@" &
  command_pid=$!
  (
    sleep "$timeout_seconds"
    echo reached > "$deadline_reached_file"
    kill -TERM -- "-$command_pid" 2>/dev/null || exit 0
    sleep "$BOUNDED_RUN_KILL_GRACE_SECONDS"
    kill -KILL -- "-$command_pid" 2>/dev/null || true
  ) &
  watchdog_pid=$!
  [[ "$job_control_was_on" == true ]] || set +m
  wait "$command_pid" || command_status=$?
  kill -KILL -- "-$watchdog_pid" 2>/dev/null || true
  wait "$watchdog_pid" 2>/dev/null || true
  if [[ ! -s "$deadline_reached_file" ]]; then
    rm -f "$deadline_reached_file"
    return "$command_status"
  fi
  rm -f "$deadline_reached_file"
  # The group leader can exit on TERM before a child that ignores TERM.
  kill -KILL -- "-$command_pid" 2>/dev/null || true
  echo "stopped after ${timeout_seconds}s: $*" >&2
  return "$BOUNDED_RUN_TIMEOUT_STATUS"
}
