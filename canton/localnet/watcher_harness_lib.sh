# Functions that build, start and stop the guardian watcher LocalNet harness
# (node/pkg/watchers/canton/watcher_localnet_test.go). Source this file after gg_lib.sh; do not run it.
# The caller sets WATCHER_DIR. Harness files go to $WATCHER_DIR/<name>.*
# WATCHER_APPROVED_CORE_PACKAGES_FILE, when set, replaces the approved list compiled into guardiand (LocalNet only).
# The harness authenticates as WATCHER_USER_ID, which holds only CanReadAs(guardianGovernance).
# shellcheck shell=bash

readonly HARNESS_READY_SECONDS=120
readonly HARNESS_STOP_SECONDS=60
readonly HARNESS_TERMINATE_SECONDS=10
readonly WATCHER_USER_ID="wormhole-watcher"
# Harness names that this shell started; stop_harness removes a name.
STARTED_HARNESS_NAMES=()

harness_binary() {
  echo "$WATCHER_DIR/canton-watcher-localnet.test"
}

build_harness() {
  mkdir -p "$WATCHER_DIR"
  (cd "$REPO_ROOT/node" && GOFLAGS=-mod=readonly GOPROXY=off go test -c -tags localnet -o "$(harness_binary)" ./pkg/watchers/canton)
}

# watcher_user_rights_json <validator>: the rights of WATCHER_USER_ID on that participant, as a sorted JSON array.
watcher_user_rights_json() {
  curl_bounded -sSf "$(json_api_url "$1")/v2/users/$WATCHER_USER_ID/rights" -H "@$ADMIN_AUTH_HEADER" \
    | jq -c '[.rights[].kind | to_entries[0] | {right: .key, party: (.value.value.party // null)}] | sort_by(.right, .party)'
}

# ensure_watcher_user <validator>: create WATCHER_USER_ID with CanReadAs(guardianGovernance) on that participant, and
# check that this is its only right.
ensure_watcher_user() {
  local validator_name="$1" http_status expected_rights
  # shellcheck disable=SC2154 # gg_lib.sh sets gg_work_dir
  http_status="$(curl_bounded -sS -o "$gg_work_dir/watcher-user.out" -w '%{http_code}' -X POST \
    "$(json_api_url "$validator_name")/v2/users" -H "@$ADMIN_AUTH_HEADER" -H 'Content-Type: application/json' \
    -d "$(jq -n --arg id "$WATCHER_USER_ID" --arg gg "$GG_PARTY" '{
      user: {id: $id, primaryParty: "", isDeactivated: false, identityProviderId: ""},
      rights: [{kind: {CanReadAs: {value: {party: $gg}}}}]}')")"
  case "$http_status" in
    200 | 409) ;;
    *)
      echo "create user $WATCHER_USER_ID on $validator_name: HTTP $http_status $(cat "$gg_work_dir/watcher-user.out")" >&2
      return 1
      ;;
  esac
  expected_rights="$(jq -nc --arg gg "$GG_PARTY" '[{right: "CanReadAs", party: $gg}]')"
  if [[ "$(watcher_user_rights_json "$validator_name")" != "$expected_rights" ]]; then
    echo "user $WATCHER_USER_ID on $validator_name has rights $(watcher_user_rights_json "$validator_name"), expected $expected_rights" >&2
    return 1
  fi
  echo "watcher user $WATCHER_USER_ID on $validator_name: rights $(watcher_user_rights_json "$validator_name" | jq -c 'map(.right)')"
}

harness_pid_is_harness() {
  local harness_pid="$1"
  [[ "$(ps -p "$harness_pid" -o command= 2>/dev/null)" == "$(harness_binary)"* ]]
}

# terminate_harness_process <name>: SIGTERM, then SIGKILL after HARNESS_TERMINATE_SECONDS. Only a pid that
# still runs the harness binary gets a signal.
terminate_harness_process() {
  local name="$1" harness_pid waited_seconds
  [[ -f "$WATCHER_DIR/$name.pid" ]] || return 0
  harness_pid="$(cat "$WATCHER_DIR/$name.pid")"
  harness_pid_is_harness "$harness_pid" || return 0
  kill -TERM "$harness_pid"
  for ((waited_seconds = 0; waited_seconds < HARNESS_TERMINATE_SECONDS; waited_seconds++)); do
    harness_pid_is_harness "$harness_pid" || return 0
    sleep 1
  done
  kill -KILL "$harness_pid"
  echo "watcher harness $name (pid $harness_pid) did not exit on SIGTERM; sent SIGKILL" >&2
}

# stop_started_harnesses: for the EXIT trap of a failed run. Terminates each harness that this shell started.
stop_started_harnesses() {
  local name
  for name in "${STARTED_HARNESS_NAMES[@]}"; do
    terminate_harness_process "$name"
    echo "watcher harness $name terminated by the EXIT trap" >&2
  done
  STARTED_HARNESS_NAMES=()
}

forget_started_harness() {
  local name="$1" kept_names=() started_name
  for started_name in "${STARTED_HARNESS_NAMES[@]}"; do
    [[ "$started_name" == "$name" ]] || kept_names+=("$started_name")
  done
  STARTED_HARNESS_NAMES=("${kept_names[@]}")
}

# run_harness <name> <core-package-id> <core-package-version> <reobserve-update-ids> <max-seconds>: run_harness_on
# SUBMITTING_VALIDATOR.
run_harness() {
  run_harness_on "$SUBMITTING_VALIDATOR" "$@"
}

# run_harness_on <validator> <name> <core-package-id> <core-package-version> <reobserve-update-ids> <max-seconds>:
# start in the background as WATCHER_USER_ID on the Ledger API of that participant, read as guardianGovernance,
# pinned to <core-package-id> at <core-package-version>.
# Returns when the harness streams. The token file is removed when the harness is ready.
run_harness_on() {
  local validator_name="$1" name="$2" core_package_id="$3" core_package_version="$4" reobserve_update_ids="$5"
  local max_seconds="$6" waited_seconds
  local token_file="$WATCHER_DIR/$name.jwt"
  ensure_watcher_user "$validator_name"
  rm -f "$WATCHER_DIR/$name".{ready,stop,jsonl,metrics.json,pid,exit,requests} "$WATCHER_DIR/watcher.jwt"
  (umask 077 && "$LOCALNET_SCRIPT_DIR/token.sh" "$WATCHER_USER_ID" > "$token_file")
  (
    CANTON_LOCALNET_GRPC="localhost:$(grpc_ledger_api_port "$validator_name")" \
    CANTON_LOCALNET_TOKEN_FILE="$token_file" \
    CANTON_LOCALNET_READ_AS_PARTY="$GG_PARTY" \
    CANTON_LOCALNET_CORE_PACKAGE_ID="$core_package_id" \
    CANTON_LOCALNET_CORE_PACKAGE_VERSION="$core_package_version" \
    CANTON_LOCALNET_APPROVED_CORE_PACKAGES_FILE="${WATCHER_APPROVED_CORE_PACKAGES_FILE:-}" \
    CANTON_LOCALNET_OBSERVATIONS_FILE="$WATCHER_DIR/$name.jsonl" \
    CANTON_LOCALNET_METRICS_FILE="$WATCHER_DIR/$name.metrics.json" \
    CANTON_LOCALNET_READY_FILE="$WATCHER_DIR/$name.ready" \
    CANTON_LOCALNET_STOP_FILE="$WATCHER_DIR/$name.stop" \
    CANTON_LOCALNET_REOBSERVE_UPDATE_IDS="$reobserve_update_ids" \
    CANTON_LOCALNET_REOBSERVE_REQUEST_FILE="$WATCHER_DIR/$name.requests" \
    CANTON_LOCALNET_MAX_SECONDS="$max_seconds" \
      nohup "$(harness_binary)" -test.run TestCantonWatcherLocalNet -test.v > "$WATCHER_DIR/$name.log" 2>&1 &
    echo $! > "$WATCHER_DIR/$name.pid"
    harness_exit_status=0
    wait $! || harness_exit_status=$?
    echo "$harness_exit_status" > "$WATCHER_DIR/$name.exit"
  ) &
  STARTED_HARNESS_NAMES+=("$name")
  for ((waited_seconds = 0; waited_seconds < HARNESS_READY_SECONDS; waited_seconds++)); do
    if [[ -f "$WATCHER_DIR/$name.ready" ]]; then
      rm -f "$token_file"
      echo "watcher harness $name streams from $validator_name as $WATCHER_USER_ID, pinned to ${core_package_id:0:12} $core_package_version (pid $(cat "$WATCHER_DIR/$name.pid" 2>/dev/null))"
      return 0
    fi
    if [[ -f "$WATCHER_DIR/$name.exit" ]]; then
      rm -f "$token_file"
      echo "watcher harness $name exited with status $(cat "$WATCHER_DIR/$name.exit"); log: $WATCHER_DIR/$name.log" >&2
      tail -n 20 "$WATCHER_DIR/$name.log" >&2
      return 1
    fi
    sleep 1
  done
  rm -f "$token_file"
  echo "watcher harness $name is not ready after ${HARNESS_READY_SECONDS}s" >&2
  return 1
}

# stop_harness <name>: create the stop file and wait for exit status 0 and a PASS line in the log.
stop_harness() {
  local name="$1" waited_seconds harness_exit_status
  touch "$WATCHER_DIR/$name.stop"
  for ((waited_seconds = 0; waited_seconds < HARNESS_STOP_SECONDS; waited_seconds++)); do
    if [[ -f "$WATCHER_DIR/$name.exit" ]]; then
      forget_started_harness "$name"
      harness_exit_status="$(cat "$WATCHER_DIR/$name.exit")"
      if [[ "$harness_exit_status" == "0" ]] && grep -q '^--- PASS: TestCantonWatcherLocalNet' "$WATCHER_DIR/$name.log"; then
        echo "watcher harness $name stopped: $(wc -l < "$WATCHER_DIR/$name.jsonl" | tr -d ' ') observations, unpinned publishes $(cat "$WATCHER_DIR/$name.metrics.json")"
        return 0
      fi
      echo "watcher harness $name failed with status $harness_exit_status; log: $WATCHER_DIR/$name.log" >&2
      grep -E -- '--- (FAIL|SKIP)|Error' "$WATCHER_DIR/$name.log" | head -n 20 >&2
      return 1
    fi
    sleep 1
  done
  terminate_harness_process "$name"
  forget_started_harness "$name"
  echo "watcher harness $name did not stop after ${HARNESS_STOP_SECONDS}s" >&2
  return 1
}

# check_harness_reobservation_errors <name>: fails on a reobservation lookup error in the harness log.
check_harness_reobservation_errors() {
  local name="$1"
  if grep -E 'canton_fetch_obvs_req (failed|invalid txID|returned a different update)' "$WATCHER_DIR/$name.log" >&2; then
    echo "watcher harness $name has failed reobservation lookups" >&2
    return 1
  fi
  echo "  watcher $name log: no 'canton_fetch_obvs_req failed', 'invalid txID' or 'returned a different update'"
}

# harness_dropped_update_ids <name> <executing-package-id>: sorted update ids of the WARN logs for
# PublishMessage events that the harness dropped because that package executed them.
harness_dropped_update_ids() {
  grep -F 'dropped PublishMessage executed by a package other than the pinned package' "$WATCHER_DIR/$1.log" \
    | grep -oE '\{.*\}' | jq -r --arg package "$2" 'select(.executingPackageId == $package) | .updateId' | sort -u
}

# canary_gate_decision <now-s> <gate-start-s> <window-start-s> <quiet-window-s> <maximum-s>: prints passed, waiting or
# timed-out. The quiet window starts at the gate start and restarts at each publish under another package. The gate
# passes when a complete quiet window ends at or before gate start + maximum. Exit status 2 on an invalid argument.
canary_gate_decision() {
  local argument
  [[ $# -eq 5 ]] || { echo "canary_gate_decision: expected 5 arguments, got $#" >&2; return 2; }
  for argument in "$@"; do
    [[ "$argument" =~ ^(0|[1-9][0-9]*)$ ]] || { echo "canary_gate_decision: not a non-negative integer: $argument" >&2; return 2; }
  done
  local now_seconds="$1" gate_start_seconds="$2" window_start_seconds="$3" quiet_window_seconds="$4" maximum_seconds="$5"
  [[ "$quiet_window_seconds" -gt 0 ]] || { echo "canary_gate_decision: the quiet window must be positive" >&2; return 2; }
  [[ "$maximum_seconds" -ge "$quiet_window_seconds" ]] \
    || { echo "canary_gate_decision: maximum $maximum_seconds is below the quiet window $quiet_window_seconds" >&2; return 2; }
  [[ "$window_start_seconds" -ge "$gate_start_seconds" ]] \
    || { echo "canary_gate_decision: window start $window_start_seconds is before the gate start $gate_start_seconds" >&2; return 2; }
  [[ "$now_seconds" -ge "$window_start_seconds" ]] \
    || { echo "canary_gate_decision: now $now_seconds is before the window start $window_start_seconds" >&2; return 2; }
  if [[ $((now_seconds - window_start_seconds)) -ge "$quiet_window_seconds" \
    && $((window_start_seconds + quiet_window_seconds)) -le $((gate_start_seconds + maximum_seconds)) ]]; then
    echo passed
  elif [[ "$now_seconds" -ge $((gate_start_seconds + maximum_seconds)) ]]; then
    echo timed-out
  else
    echo waiting
  fi
}

# request_reobservations <name> <update-ids, one per line>: a reobservation request to the running harness <name>.
# The harness reads the request file every 500 ms. The rename keeps each read of the file complete.
request_reobservations() {
  local request_file="$WATCHER_DIR/$1.requests" partial_file
  partial_file="$(mktemp "$request_file.XXXXXX")"
  { [[ ! -f "$request_file" ]] || cat "$request_file"; printf '%s\n' "$2"; } > "$partial_file"
  mv -f "$partial_file" "$request_file"
}
