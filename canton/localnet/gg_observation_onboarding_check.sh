#!/usr/bin/env bash
# Observation-first onboarding check (L1 runbook rule 3): a guardianGovernance host at Observation stays below the
# confirmation threshold; after promotion to Confirmation it counts.
#   1. Set guardian4 to Observation. Disconnect app-provider. guardian4 must be connected. A gg PublishMessage with only
#      sv confirming must time out, with guardianGovernance as the one unresponsive party, and the mediator
#      log must show no guardian4 verdict in that time.
#   2. Promote guardian4 to Confirmation. Disconnect app-provider. The same PublishMessage must commit, because sv and
#      guardian4 reach the threshold of 2. CorePublisher submits on sv, so sv stays connected.
# Precondition: guardian4 hosts guardianGovernance at Confirmation (gg_hosting_change_check.sh passes) and an
# CorePublisher Emitter exists (gg_publish_and_govern.sh ran). The mediator timeout runs on sequencer time, so a
# background ticker requests time proofs from sv while app-provider is offline. The EXIT trap reconnects app-provider
# and restores guardian4 to Confirmation.
# Usage: gg_observation_onboarding_check.sh [owner-name owner-name]
set -euo pipefail
# shellcheck source=gg_lib.sh
source "$(dirname "${BASH_SOURCE[0]}")/gg_lib.sh"

readonly OBSERVATION_TICKER_SECONDS=600
readonly GUARDIAN4_JSON_API_URL="http://localhost:5975"
first_owner="${1:-owner1}"
second_owner="${2:-owner2}"
check_owner_names_are_safe "$first_owner" "$second_owner"
check_owner_names_differ "$first_owner" "$second_owner"

set_guardian4_permission() {
  REPLICATION_PERMISSION_HOST=guardian4 REPLICATION_HOST_PERMISSION="$1" \
    "$LOCALNET_SCRIPT_DIR/party_replication.sh" set-permission "$first_owner" "$second_owner" > "$gg_work_dir/set-permission.log" 2>&1 \
    || { cat "$gg_work_dir/set-permission.log" >&2; echo "FAIL: cannot set guardian4 to $1" >&2; return 1; }
  echo "guardian4 permission set to $1"
}

restore_state() {
  local exit_status=$?
  stop_clock_ticker
  if ! set_participant_connection reconnect app-provider; then
    echo "ERROR: app-provider stays disconnected; reconnect with participant_connection.canton" >&2
    exit_status=1
  fi
  set_guardian4_permission Confirmation || exit_status=1
  rm -rf "$gg_work_dir"
  exit "$exit_status"
}
trap restore_state EXIT

# require_timeout_rejection <output-prefix>: the completion in <prefix>.out must be MEDIATOR_SAYS_TX_TIMED_OUT with
# guardianGovernance as the only unresponsive party. feeCustody keeps two connected hosts: sv and app-user.
require_timeout_rejection() {
  local summary
  summary="$(completion_summary "$1.out" 2>/dev/null || echo '{}')"
  jq -e '.message | test("^MEDIATOR_SAYS_TX_TIMED_OUT")' <<<"$summary" >/dev/null 2>&1 \
    || { echo "FAIL: expected MEDIATOR_SAYS_TX_TIMED_OUT" >&2; cat "$1.out" "$1.err" >&2; exit 1; }
  jq -e --arg party "$GG_PARTY" '(.unresponsiveParties // "") | split(",") | map(select(length > 0)) == [$party]' <<<"$summary" >/dev/null 2>&1 \
    || { echo "FAIL: expected unresponsiveParties to be exactly $GG_PARTY" >&2; cat "$1.out" "$1.err" >&2; exit 1; }
  echo "PASS: Observation host does not count; the PublishMessage timed out. $summary"
}

# require_no_guardian4_verdict <since>: the mediator log has no verdict of guardian4 since <since>. guardian4 hosts only
# the guardian-namespace parties, so this covers the timed-out request.
require_no_guardian4_verdict() {
  local verdict_lines guardian4_receipt
  verdict_lines="$(docker logs canton --since "$1" 2>&1 \
    | jq -rR 'fromjson? | select(.message | test("Received verdict .* by participant PAR::guardian4::")) | .message | .[0:260]')"
  if [[ -n "$verdict_lines" ]]; then
    echo "FAIL: the mediator received a guardian4 verdict at Observation:" >&2
    head -n 5 <<<"$verdict_lines" >&2
    exit 1
  fi
  echo "PASS: the mediator log has no guardian4 verdict since $1"
  guardian4_receipt="$(docker logs guardian4 --since "$1" 2>&1 | grep -m1 -oE 'verdict=RequestId\([^)]*\)[^(]{0,80}' || true)"
  if [[ -n "$guardian4_receipt" ]]; then
    echo "PASS: guardian4 received the request and the mediator result: $guardian4_receipt"
  else
    echo "INFO: the guardian4 log shows no mediator result line since $1"
  fi
}

echo "Step 1: guardian4 at Observation, app-provider disconnected. A gg PublishMessage with one confirming host must time out."
set_guardian4_permission Observation || fail "cannot set guardian4 to Observation"
set_participant_connection disconnect app-provider || fail "cannot disconnect app-provider"
check_connected_to_gg_synchronizer "$GUARDIAN4_JSON_API_URL" guardian4 || fail "guardian4 must be connected for the Observation step"
echo "PASS: guardian4 is connected to $LOGICAL_SYNCHRONIZER_ID"
start_clock_ticker "$OBSERVATION_TICKER_SECONDS" sv
observation_started_at="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
SUBMIT_AWAIT_COMPLETION=true publish_message "observation-one-host-$(date +%s)" "6f627365727665" \
  > "$gg_work_dir/observe.out" 2> "$gg_work_dir/observe.err" || true
require_timeout_rejection "$gg_work_dir/observe"
require_no_guardian4_verdict "$observation_started_at"
stop_clock_ticker
set_participant_connection reconnect app-provider || fail "cannot reconnect app-provider after step 1"

echo "Step 2: promote guardian4 to Confirmation, app-provider disconnected. The same PublishMessage must commit."
set_guardian4_permission Confirmation || fail "cannot promote guardian4 to Confirmation"
set_participant_connection disconnect app-provider || fail "cannot disconnect app-provider"
promote_transaction="$(publish_message "observation-promoted-$(date +%s)" "70726f6d6f7465")" \
  || fail "the PublishMessage did not commit after promotion to Confirmation"
set_participant_connection reconnect app-provider || fail "cannot reconnect app-provider after step 2"
echo "PASS: Confirmation host counts; the PublishMessage committed, update $(jq -er .updateId <<<"$promote_transaction")"
echo "PASS: gg_observation_onboarding_check"
