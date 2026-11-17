#!/usr/bin/env bash
# Run the guardian watcher (node/pkg/watchers/canton) against app-provider as guardianGovernance with the
# pinned wormhole-core package id, and check that it observes the NTT transceiver messages.
# Usage:
#   ntt_watcher.sh start      Build the harness and start it in the background. Returns when it streams.
#   ntt_watcher.sh stop       Stop the background harness.
#   ntt_watcher.sh check      Check the live observations against ntt-contracts.json.
#   ntt_watcher.sh reobserve  Reobserve the recorded outbound transfers and check them.
# Precondition for check and reobserve: ntt_burn_mint.sh and ntt_lock_unlock.sh ran.
set -euo pipefail
# shellcheck source=ntt_lib.sh
source "$(dirname "${BASH_SOURCE[0]}")/ntt_lib.sh"

readonly CORE_PACKAGE_ID="701a57c867e02d962f231bd9f0ff5186e16462a77289aa1b7aaba7fe95074c15"
readonly WATCHER_DIR="$LOCALNET_STATE_DIR/ntt-watcher"
readonly HARNESS_BINARY="$WATCHER_DIR/canton-watcher-localnet.test"
readonly HARNESS_READY_SECONDS=120
readonly HARNESS_STOP_SECONDS=60
mkdir -p "$WATCHER_DIR"

build_harness() {
  (cd "$REPO_ROOT/node" && GOFLAGS=-mod=readonly GOPROXY=off go test -c -tags localnet -o "$HARNESS_BINARY" ./pkg/watchers/canton)
}

# run_harness <name> <reobserve-offsets> <max-seconds>: run in the background; files go to $WATCHER_DIR/<name>.*
run_harness() {
  local name="$1"
  rm -f "$WATCHER_DIR/$name.ready" "$WATCHER_DIR/$name.stop" "$WATCHER_DIR/$name.jsonl"
  "$LOCALNET_SCRIPT_DIR/token.sh" "$SCRIPT_USER_ID" > "$WATCHER_DIR/watcher.jwt"
  CANTON_LOCALNET_GRPC="localhost:$(grpc_ledger_api_port "$SUBMITTING_VALIDATOR")" \
  CANTON_LOCALNET_TOKEN_FILE="$WATCHER_DIR/watcher.jwt" \
  CANTON_LOCALNET_READ_AS_PARTY="$GG_PARTY" \
  CANTON_LOCALNET_CORE_PACKAGE_ID="$CORE_PACKAGE_ID" \
  CANTON_LOCALNET_OBSERVATIONS_FILE="$WATCHER_DIR/$name.jsonl" \
  CANTON_LOCALNET_READY_FILE="$WATCHER_DIR/$name.ready" \
  CANTON_LOCALNET_STOP_FILE="$WATCHER_DIR/$name.stop" \
  CANTON_LOCALNET_REOBSERVE_OFFSETS="$2" \
  CANTON_LOCALNET_MAX_SECONDS="$3" \
    nohup "$HARNESS_BINARY" -test.run TestCantonWatcherLocalNet -test.v > "$WATCHER_DIR/$name.log" 2>&1 &
  echo $! > "$WATCHER_DIR/$name.pid"
  local waited_seconds
  for ((waited_seconds = 0; waited_seconds < HARNESS_READY_SECONDS; waited_seconds++)); do
    if [[ -f "$WATCHER_DIR/$name.ready" ]]; then
      echo "watcher harness $name streams from app-provider (pid $(cat "$WATCHER_DIR/$name.pid"))"
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

# participant_offset <validator> <update-id>: the offset of that update on the validator's participant.
# Offsets are participant-local; the watcher TxID encodes the offset of the participant it reads.
participant_offset() {
  curl -sSf -X POST "$(json_api_url "$1")/v2/updates/update-by-id" -H "Authorization: Bearer $(cat "$gg_work_dir/script.jwt")" \
    -H 'Content-Type: application/json' \
    -d "$(jq -n --arg id "$2" --arg gg "$GG_PARTY" '{updateId: $id, updateFormat: {includeTransactions: {
      eventFormat: {filtersByParty: {($gg): {cumulative: [{identifierFilter: {WildcardFilter: {value: {includeCreatedEventBlob: false}}}}]}}, verbose: false},
      transactionShape: "TRANSACTION_SHAPE_ACS_DELTA"}}}')" \
    | jq -er '.update.Transaction.value.offset'
}

case "${1:-}" in
  start)
    build_harness
    run_harness live "" 1800
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
    burn_offset="$(participant_offset "$SUBMITTING_VALIDATOR" "$(jq -r .burnMint.burnUpdateId "$NTT_CONTRACTS")")"
    lock_offset="$(participant_offset "$SUBMITTING_VALIDATOR" "$(jq -r .lockUnlock.lockUpdateId "$NTT_CONTRACTS")")"
    echo "app-provider offsets: burn $burn_offset, lock $lock_offset"
    run_harness reobserve "$burn_offset,$lock_offset" 300
    stop_harness reobserve
    check_deployment "$WATCHER_DIR/reobserve.jsonl" burnMint burnMessage true
    check_deployment "$WATCHER_DIR/reobserve.jsonl" lockUnlock lockMessage true
    ;;
  *)
    echo "usage: ntt_watcher.sh start|stop|check|reobserve" >&2
    exit 2
    ;;
esac
