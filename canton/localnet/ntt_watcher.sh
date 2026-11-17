#!/usr/bin/env bash
# Run the guardian watcher (node/pkg/watchers/canton) against app-provider as guardianGovernance with the
# pinned wormhole-core package id and version, and check that it observes the NTT transceiver messages.
# Usage:
#   ntt_watcher.sh start      Build the harness and start it in the background. Returns when it streams.
#   ntt_watcher.sh stop       Stop the background harness.
#   ntt_watcher.sh check      Check the live observations against ntt-contracts.json.
#   ntt_watcher.sh reobserve  Reobserve the recorded outbound transfers and check them.
# Precondition for check and reobserve: ntt_burn_mint.sh and ntt_lock_unlock.sh ran.
set -euo pipefail
# shellcheck source=ntt_lib.sh
source "$(dirname "${BASH_SOURCE[0]}")/ntt_lib.sh"

# The wormhole-core package that executes the NTT PublishMessage exercises. Default: the pin of ntt_lib.sh.
readonly CORE_PACKAGE_ID="${NTT_CORE_PACKAGE_ID:-$WORMHOLE_CORE_PACKAGE_ID}"
readonly WATCHER_DIR="$LOCALNET_STATE_DIR/ntt-watcher"
# shellcheck source=watcher_harness_lib.sh
source "$LOCALNET_SCRIPT_DIR/watcher_harness_lib.sh"
mkdir -p "$WATCHER_DIR"

# A failed run terminates the harness that it started. "start" leaves its harness running when it passes.
on_exit() {
  local exit_status=$?
  if [[ "$exit_status" -ne 0 ]]; then stop_started_harnesses; fi
  rm -rf "$gg_work_dir"
}
trap on_exit EXIT

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
    run_harness live "$CORE_PACKAGE_ID" "" 1800
    ;;
  stop)
    stop_harness live
    ;;
  check)
    check_deployment "$WATCHER_DIR/live.jsonl" burnMint burnMessage false
    check_deployment "$WATCHER_DIR/live.jsonl" lockUnlock lockMessage false
    ;;
  reobserve)
    [[ -x "$(harness_binary)" ]] || build_harness
    burn_update_id="$(jq -er .burnMint.burnUpdateId "$NTT_CONTRACTS")"
    lock_update_id="$(jq -er .lockUnlock.lockUpdateId "$NTT_CONTRACTS")"
    echo "reobservation update ids: burn $burn_update_id, lock $lock_update_id"
    run_harness reobserve "$CORE_PACKAGE_ID" "$burn_update_id,$lock_update_id" 300
    stop_harness reobserve
    check_harness_reobservation_errors reobserve
    check_deployment "$WATCHER_DIR/reobserve.jsonl" burnMint burnMessage true
    check_deployment "$WATCHER_DIR/reobserve.jsonl" lockUnlock lockMessage true
    ;;
  *)
    echo "usage: ntt_watcher.sh start|stop|check|reobserve" >&2
    exit 2
    ;;
esac
