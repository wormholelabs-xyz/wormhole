# Functions that build, start and stop the guardian watcher LocalNet harness
# (node/pkg/watchers/canton/watcher_localnet_test.go). Source this file after gg_lib.sh; do not run it.
# The caller sets WATCHER_DIR. Harness files go to $WATCHER_DIR/<name>.*
# shellcheck shell=bash

readonly HARNESS_READY_SECONDS=120
readonly HARNESS_STOP_SECONDS=60

harness_binary() {
  echo "$WATCHER_DIR/canton-watcher-localnet.test"
}

build_harness() {
  mkdir -p "$WATCHER_DIR"
  (cd "$REPO_ROOT/node" && GOFLAGS=-mod=mod GOPROXY=off go test -c -tags localnet -o "$(harness_binary)" ./pkg/watchers/canton)
}

# run_harness <name> <core-package-id> <reobserve-update-ids> <max-seconds>: start in the background as
# guardianGovernance on app-provider, pinned to <core-package-id>. Returns when the harness streams.
run_harness() {
  local name="$1" core_package_id="$2" reobserve_update_ids="$3" max_seconds="$4" waited_seconds
  rm -f "$WATCHER_DIR/$name.ready" "$WATCHER_DIR/$name.stop" "$WATCHER_DIR/$name.jsonl"
  (umask 077 && "$LOCALNET_SCRIPT_DIR/token.sh" "$SCRIPT_USER_ID" > "$WATCHER_DIR/watcher.jwt")
  CANTON_LOCALNET_GRPC="localhost:$(grpc_ledger_api_port "$SUBMITTING_VALIDATOR")" \
  CANTON_LOCALNET_TOKEN_FILE="$WATCHER_DIR/watcher.jwt" \
  CANTON_LOCALNET_READ_AS_PARTY="$GG_PARTY" \
  CANTON_LOCALNET_CORE_PACKAGE_ID="$core_package_id" \
  CANTON_LOCALNET_OBSERVATIONS_FILE="$WATCHER_DIR/$name.jsonl" \
  CANTON_LOCALNET_READY_FILE="$WATCHER_DIR/$name.ready" \
  CANTON_LOCALNET_STOP_FILE="$WATCHER_DIR/$name.stop" \
  CANTON_LOCALNET_REOBSERVE_UPDATE_IDS="$reobserve_update_ids" \
  CANTON_LOCALNET_MAX_SECONDS="$max_seconds" \
    nohup "$(harness_binary)" -test.run TestCantonWatcherLocalNet -test.v > "$WATCHER_DIR/$name.log" 2>&1 &
  echo $! > "$WATCHER_DIR/$name.pid"
  for ((waited_seconds = 0; waited_seconds < HARNESS_READY_SECONDS; waited_seconds++)); do
    if [[ -f "$WATCHER_DIR/$name.ready" ]]; then
      echo "watcher harness $name streams from $SUBMITTING_VALIDATOR, pinned to ${core_package_id:0:12} (pid $(cat "$WATCHER_DIR/$name.pid"))"
      return 0
    fi
    if ! kill -0 "$(cat "$WATCHER_DIR/$name.pid")" 2>/dev/null; then
      echo "watcher harness $name exited; log: $WATCHER_DIR/$name.log" >&2
      tail -n 20 "$WATCHER_DIR/$name.log" >&2
      return 1
    fi
    sleep 1
  done
  echo "watcher harness $name is not ready after ${HARNESS_READY_SECONDS}s" >&2
  return 1
}

# stop_harness <name>: create the stop file and wait for a clean exit.
stop_harness() {
  local name="$1" harness_pid waited_seconds
  harness_pid="$(cat "$WATCHER_DIR/$name.pid")"
  touch "$WATCHER_DIR/$name.stop"
  for ((waited_seconds = 0; waited_seconds < HARNESS_STOP_SECONDS; waited_seconds++)); do
    if ! kill -0 "$harness_pid" 2>/dev/null; then
      if grep -q '^--- PASS: TestCantonWatcherLocalNet' "$WATCHER_DIR/$name.log"; then
        echo "watcher harness $name stopped: $(wc -l < "$WATCHER_DIR/$name.jsonl" | tr -d ' ') observations"
        return 0
      fi
      echo "watcher harness $name failed; log: $WATCHER_DIR/$name.log" >&2
      grep -E -- '--- FAIL|Error' "$WATCHER_DIR/$name.log" | head -n 20 >&2
      return 1
    fi
    sleep 1
  done
  kill "$harness_pid" 2>/dev/null || true
  echo "watcher harness $name did not stop after ${HARNESS_STOP_SECONDS}s" >&2
  return 1
}
