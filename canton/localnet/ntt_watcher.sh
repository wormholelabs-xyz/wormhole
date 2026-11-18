#!/usr/bin/env bash
# Run the guardian watcher (node/pkg/watchers/canton) against app-provider as guardianGovernance with the
# pinned wormhole-core package id and version, and check that it observes the NTT transceiver messages.
# Usage:
#   ntt_watcher.sh start      Build the harness and start it in the background. Returns when it streams.
#   ntt_watcher.sh stop       Stop the background harness.
#   ntt_watcher.sh check      Check the live observations against ntt-contracts.json.
#   ntt_watcher.sh reobserve  Reobserve the recorded outbound transfers on app-provider and on sv, and check them.
# Precondition for check and reobserve: ntt_burn_mint.sh and ntt_lock_unlock.sh ran.
set -euo pipefail
# shellcheck source=ntt_lib.sh
source "$(dirname "${BASH_SOURCE[0]}")/ntt_lib.sh"

readonly WATCHER_DIR="$LOCALNET_STATE_DIR/ntt-watcher"
readonly HARNESS_BINARY="$WATCHER_DIR/canton-watcher-localnet.test"
readonly HARNESS_READY_SECONDS=120
readonly HARNESS_STOP_SECONDS=60
mkdir -p "$WATCHER_DIR"

build_harness() {
  (cd "$REPO_ROOT/node" && GOFLAGS=-mod=readonly GOPROXY=off go test -c -tags localnet -o "$HARNESS_BINARY" ./pkg/watchers/canton)
}

# run_harness <name> <validator> <reobserve-update-ids> <max-seconds>: run in the background against the
# participant of <validator>; files go to $WATCHER_DIR/<name>.*, the exit status to <name>.exit.
run_harness() {
  local name="$1" validator_name="$2" waited_seconds
  rm -f "$WATCHER_DIR/$name".{ready,stop,jsonl,pid,exit}
  (umask 077 && "$LOCALNET_SCRIPT_DIR/token.sh" "$SCRIPT_USER_ID" > "$WATCHER_DIR/watcher.jwt")
  (
    CANTON_LOCALNET_GRPC="localhost:$(grpc_ledger_api_port "$validator_name")" \
    CANTON_LOCALNET_TOKEN_FILE="$WATCHER_DIR/watcher.jwt" \
    CANTON_LOCALNET_READ_AS_PARTY="$GG_PARTY" \
    CANTON_LOCALNET_CORE_PACKAGE_ID="$WORMHOLE_CORE_PACKAGE_ID" \
    CANTON_LOCALNET_CORE_PACKAGE_VERSION="$WORMHOLE_CORE_VERSION" \
    CANTON_LOCALNET_OBSERVATIONS_FILE="$WATCHER_DIR/$name.jsonl" \
    CANTON_LOCALNET_READY_FILE="$WATCHER_DIR/$name.ready" \
    CANTON_LOCALNET_STOP_FILE="$WATCHER_DIR/$name.stop" \
    CANTON_LOCALNET_REOBSERVE_UPDATE_IDS="$3" \
    CANTON_LOCALNET_MAX_SECONDS="$4" \
      nohup "$HARNESS_BINARY" -test.run TestCantonWatcherLocalNet -test.v > "$WATCHER_DIR/$name.log" 2>&1 &
    echo $! > "$WATCHER_DIR/$name.pid"
    harness_exit_status=0
    wait $! || harness_exit_status=$?
    echo "$harness_exit_status" > "$WATCHER_DIR/$name.exit"
  ) &
  for ((waited_seconds = 0; waited_seconds < HARNESS_READY_SECONDS; waited_seconds++)); do
    if [[ -f "$WATCHER_DIR/$name.ready" ]]; then
      echo "watcher harness $name streams from $validator_name (pid $(cat "$WATCHER_DIR/$name.pid"))"
      return 0
    fi
    if [[ -f "$WATCHER_DIR/$name.exit" ]]; then
      echo "watcher harness $name exited with status $(cat "$WATCHER_DIR/$name.exit"); log: $WATCHER_DIR/$name.log" >&2
      tail -n 20 "$WATCHER_DIR/$name.log" >&2
      return 1
    fi
    sleep 1
  done
  echo "watcher harness $name is not ready after ${HARNESS_READY_SECONDS}s" >&2
  return 1
}

# stop_harness <name>: create the stop file and wait for exit status 0 and a PASS line in the log.
stop_harness() {
  local name="$1" waited_seconds harness_exit_status
  touch "$WATCHER_DIR/$name.stop"
  for ((waited_seconds = 0; waited_seconds < HARNESS_STOP_SECONDS; waited_seconds++)); do
    if [[ -f "$WATCHER_DIR/$name.exit" ]]; then
      harness_exit_status="$(cat "$WATCHER_DIR/$name.exit")"
      if [[ "$harness_exit_status" == "0" ]] && grep -q '^--- PASS: TestCantonWatcherLocalNet' "$WATCHER_DIR/$name.log"; then
        echo "watcher harness $name stopped: $(wc -l < "$WATCHER_DIR/$name.jsonl" | tr -d ' ') observations"
        return 0
      fi
      echo "watcher harness $name failed with status $harness_exit_status; log: $WATCHER_DIR/$name.log" >&2
      grep -E -- '--- (FAIL|SKIP)|Error' "$WATCHER_DIR/$name.log" | head -n 20 >&2
      return 1
    fi
    sleep 1
  done
  kill "$(cat "$WATCHER_DIR/$name.pid")" 2>/dev/null || true
  echo "watcher harness $name did not stop after ${HARNESS_STOP_SECONDS}s" >&2
  return 1
}

# check_deployment <observations-file> <deployment-key> <outbound-message-key> <reobserved: true|false>
# The deployment emitter publishes TransceiverInit (sequence 0), TransceiverRegistration (1) and the transfer (2).
check_deployment() {
  local observations_file="$1" deployment_key="$2" message_key="$3" reobserved="$4" deployment
  deployment="$(jq -c --arg key "$deployment_key" '.[$key]' "$NTT_CONTRACTS")"
  jq -se --argjson deployment "$deployment" --arg messageKey "$message_key" --argjson reobserved "$reobserved" '
    ($deployment[$messageKey]) as $message
    | [.[] | select(.emitterAddress == $deployment.emitterAddress and .isReobservation == $reobserved)] as $observed
    | ($observed | map(select(.sequence == ($message.sequence | tonumber)))) as $transfer
    | if ($transfer | length) != 1 then error("expected one observation of sequence \($message.sequence), found \($transfer | length)")
      elif $transfer[0].emitterChain != 75 then error("emitter chain \($transfer[0].emitterChain), expected 75")
      elif $transfer[0].payloadHex != $message.payload then error("observed payload differs from the WormholeMessage")
      elif $transfer[0].nonce != ($message.nonce | tonumber) then error("observed nonce differs")
      elif ($reobserved | not) and ([$observed[].payloadHex[0:8]] | sort) != ["18fc67c2", "9945ff10", "9c23bd3b"]
        then error("live payload prefixes \([$observed[].payloadHex[0:8]]), expected init, registration and transfer")
      else true end' "$observations_file" >/dev/null
  echo "$deployment_key: watcher observed chain 75, emitter $(jq -r .emitterAddress <<<"$deployment"), sequence $(jq -r --arg key "$message_key" '.[$key].sequence' <<<"$deployment"), payload equal to the WormholeMessage (reobservation $reobserved)"
}

case "${1:-}" in
  start)
    build_harness
    run_harness live "$SUBMITTING_VALIDATOR" "" 1800
    ;;
  stop)
    stop_harness live
    ;;
  check)
    check_deployment "$WATCHER_DIR/live.jsonl" burnMint burnMessage false
    check_deployment "$WATCHER_DIR/live.jsonl" lockUnlock lockMessage false
    ;;
  reobserve)
    [[ -x "$HARNESS_BINARY" ]] || build_harness
    burn_update_id="$(jq -er .burnMint.burnUpdateId "$NTT_CONTRACTS")"
    lock_update_id="$(jq -er .lockUnlock.lockUpdateId "$NTT_CONTRACTS")"
    echo "reobservation update ids: burn $burn_update_id, lock $lock_update_id"
    # The update id is the same on each participant, so a request from one guardian works on another.
    for reobserving_validator in "$SUBMITTING_VALIDATOR" sv; do
      run_harness "reobserve-$reobserving_validator" "$reobserving_validator" "$burn_update_id,$lock_update_id" 300
      stop_harness "reobserve-$reobserving_validator"
      check_deployment "$WATCHER_DIR/reobserve-$reobserving_validator.jsonl" burnMint burnMessage true
      check_deployment "$WATCHER_DIR/reobserve-$reobserving_validator.jsonl" lockUnlock lockMessage true
    done
    ;;
  *)
    echo "usage: ntt_watcher.sh start|stop|check|reobserve" >&2
    exit 2
    ;;
esac
