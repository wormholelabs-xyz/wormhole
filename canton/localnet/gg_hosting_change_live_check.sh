#!/usr/bin/env bash
# Live checks of the guardian replacement rehearsal (L1): a PublishMessage commits with guardian4 as a confirmer,
# including with sv disconnected. Each current host shows the message; the rehearsal removed app-user as a host, so the
# check requires its update offset to be absent.
# Usage: gg_hosting_change_live_check.sh
set -euo pipefail
# shellcheck source=gg_lib.sh
source "$(dirname "${BASH_SOURCE[0]}")/gg_lib.sh"

readonly PUBLISH_MESSAGE_SCAN="$REPO_ROOT/canton/tools/publish_message_scan.sh"
readonly GUARDIAN4_JSON_API_URL="http://localhost:5975"

ledger_end_at() {
  curl_bounded -sSf "$1/v2/state/ledger-end" -H "@$SCRIPT_AUTH_HEADER" | jq -er .offset
}
# gg_update_offset_at <json-api-url> <update-id>: the local offset of a guardianGovernance update on one participant,
# or "ABSENT" when that participant has no such update. publish_message_scan.sh takes this offset as its end offset,
# where the required update must sit.
gg_update_offset_at() {
  local event_format request_body
  event_format="$(jq -n --arg party "$GG_PARTY" \
    '{filtersByParty: {($party): {cumulative: [{identifierFilter: {WildcardFilter: {value: {includeCreatedEventBlob: false}}}}]}}, verbose: false}')"
  request_body="$(jq -n --arg id "$2" --argjson eventFormat "$event_format" \
    '{updateId: $id, updateFormat: {includeTransactions: {eventFormat: $eventFormat, transactionShape: "TRANSACTION_SHAPE_LEDGER_EFFECTS"}}}')"
  curl_bounded -sS -X POST "$1/v2/updates/update-by-id" -H "@$SCRIPT_AUTH_HEADER" -H 'Content-Type: application/json' -d "$request_body" \
    | jq -r '.update.Transaction.value.offset // "ABSENT"'
}
# scan_confirms_publish <label> <participant-name> <json-api-url> <begin-exclusive> <update-id>: the participant shows the
# update as a guardianGovernance PublishMessage, and publish_message_scan.sh confirms it at its own offset on that participant.
scan_confirms_publish() {
  local label="$1" participant_name="$2" url="$3" begin="$4" update_id="$5" end
  end="$(gg_update_offset_at "$url" "$update_id")"
  [[ "$end" =~ ^[0-9]+$ ]] || fail "$label: $participant_name does not show update $update_id as guardianGovernance"
  "$PUBLISH_MESSAGE_SCAN" "$url" "$gg_work_dir/script.jwt" "$GG_PARTY" "$begin" "$end" "$update_id" >/dev/null \
    || fail "$label: publish_message_scan on $participant_name did not confirm update $update_id at offset $end"
  echo "PASS: $label: $participant_name shows the PublishMessage as guardianGovernance"
}
# publish_and_check <label> <require-guardian4-vote>: publish one message and check it on each host; app-user's update
# offset must be absent. With require-guardian4-vote true, also require that the mediator counted a guardian4 approval
# for guardianGovernance. guardian4 is the third of three confirmers at threshold 2, so with every host connected the
# mediator can finalize on the other two before guardian4's vote counts; the vote is required only when sv is
# disconnected and guardian4 is one of the two reachable confirmers.
publish_and_check() {
  local label="$1" require_guardian4_vote="$2" guardian4_begin provider_begin started_at transaction update_id user_offset
  guardian4_begin="$(ledger_end_at "$GUARDIAN4_JSON_API_URL")"
  provider_begin="$(ledger_end_at "$(json_api_url app-provider)")"
  started_at="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  transaction="$(publish_message "hosting-change-$label-$(date +%s)" "c0ffee$(printf '%02x' "${#label}")")" \
    || fail "$label: PublishMessage did not commit"
  update_id="$(jq -er .updateId <<<"$transaction")"
  echo "PASS: $label: PublishMessage committed, update $update_id"
  scan_confirms_publish "$label" guardian4 "$GUARDIAN4_JSON_API_URL" "$guardian4_begin" "$update_id"
  scan_confirms_publish "$label" app-provider "$(json_api_url app-provider)" "$provider_begin" "$update_id"
  user_offset="$(gg_update_offset_at "$(json_api_url app-user)" "$update_id")"
  [[ "$user_offset" == ABSENT ]] || fail "$label: app-user still receives guardianGovernance transactions (offset $user_offset)"
  echo "PASS: $label: app-user has no offset for the PublishMessage"
  if [[ "$require_guardian4_vote" == true ]]; then
    docker logs canton --since "$started_at" 2>&1 \
      | jq -rR 'fromjson? | select(.message | test("Received verdict LocalApprove\\(\\) for pending parties .*guardianGovernance")) | .message | .[0:260]' \
      | grep "by participant PAR::guardian4::" | head -n 1 | grep . \
      || fail "$label: the mediator log has no guardian4 approval for guardianGovernance"
    echo "PASS: $label: guardian4 approved for guardianGovernance"
  fi
}

publish_and_check all-hosts-connected false
set_participant_connection disconnect sv || fail "cannot disconnect sv"
trap 'set_participant_connection reconnect sv || echo "ERROR: sv stays disconnected; reconnect it with participant_connection.canton"; rm -rf "$gg_work_dir"' EXIT
publish_and_check sv-disconnected true
echo "PASS: gg_hosting_change_live_check"
