#!/usr/bin/env bash
# Live checks of the guardian replacement rehearsal (L1): a PublishMessage commits with guardian4 as a confirmer,
# including with app-provider disconnected. CorePublisher submits on sv, so sv stays connected. Each current host shows the message; the rehearsal removed app-user as a host, so the
# check requires that app-user shows no Emitter event of it.
# Usage: gg_hosting_change_live_check.sh
set -euo pipefail
# shellcheck source=gg_lib.sh
source "$(dirname "${BASH_SOURCE[0]}")/gg_lib.sh"

readonly PUBLISH_MESSAGE_SCAN="$REPO_ROOT/canton/tools/publish_message_scan.sh"
readonly GUARDIAN4_JSON_API_URL="http://localhost:5975"

ledger_end_at() {
  curl_bounded -sSf "$1/v2/state/ledger-end" -H "@$SCRIPT_AUTH_HEADER" | jq -er .offset
}
readonly UPDATE_VISIBLE_MAX_ATTEMPTS=20
# gg_update_at <json-api-url> <update-id>: the update as the participant shows it with a guardianGovernance filter, or
# "ABSENT" on HTTP 404 with UPDATE_NOT_FOUND. Any other response fails.
gg_update_at() {
  local event_format request_body response_file http_status
  event_format="$(jq -n --arg party "$GG_PARTY" \
    '{filtersByParty: {($party): {cumulative: [{identifierFilter: {WildcardFilter: {value: {includeCreatedEventBlob: false}}}}]}}, verbose: false}')"
  request_body="$(jq -n --arg id "$2" --argjson eventFormat "$event_format" \
    '{updateId: $id, updateFormat: {includeTransactions: {eventFormat: $eventFormat, transactionShape: "TRANSACTION_SHAPE_LEDGER_EFFECTS"}}}')"
  response_file="$(mktemp "$gg_work_dir/update-by-id-XXXXXX")"
  http_status="$(curl_bounded -sS -o "$response_file" -w '%{http_code}' -X POST "$1/v2/updates/update-by-id" \
    -H "@$SCRIPT_AUTH_HEADER" -H 'Content-Type: application/json' -d "$request_body")" \
    || fail "update-by-id $2 at $1: request failed"
  case "$http_status" in
    200) jq -ec '.update.Transaction.value | select(.offset != null)' "$response_file" \
      || fail "update-by-id $2 at $1: HTTP 200 without a transaction offset" ;;
    404)
      jq -e '.code == "UPDATE_NOT_FOUND"' "$response_file" >/dev/null \
        || fail "update-by-id $2 at $1: HTTP 404 without UPDATE_NOT_FOUND: $(head -c 400 "$response_file")"
      echo ABSENT ;;
    *) fail "update-by-id $2 at $1: HTTP $http_status: $(head -c 400 "$response_file")" ;;
  esac
}
# gg_update_offset_at <json-api-url> <update-id>: the local offset of the update, waiting at most
# UPDATE_VISIBLE_MAX_ATTEMPTS seconds for the participant to show it. publish_message_scan.sh takes this offset as its
# end offset, where the required update must sit.
gg_update_offset_at() {
  local attempt update_json
  for ((attempt = 1; attempt <= UPDATE_VISIBLE_MAX_ATTEMPTS; attempt++)); do
    update_json="$(gg_update_at "$1" "$2")" || return 1
    if [[ "$update_json" != ABSENT ]]; then
      jq -er .offset <<<"$update_json" || return 1
      return 0
    fi
    sleep 1
  done
  echo ABSENT
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
# check_app_user_has_no_emitter_event <label> <update-id>: app-user shows no Emitter event of the update. app-user hosts
# feeCustody, a CoreState stakeholder, so it can show the CoreState events of the update.
check_app_user_has_no_emitter_event() {
  local update_json event_templates
  update_json="$(gg_update_at "$(json_api_url app-user)" "$2")"
  if [[ "$update_json" == ABSENT ]]; then
    echo "PASS: $1: app-user has no offset for the PublishMessage"
    return 0
  fi
  event_templates="$(jq -r '[.events[] | to_entries[0] | "\(.value.templateId | split(":") | .[1:] | join(":")).\(.value.choice // "create")"] | join(",")' \
    <<<"$update_json")"
  if grep -qE '(^|,)Wormhole\.Core\.State:Emitter\.' <<<"$event_templates"; then
    fail "$1: app-user shows the Emitter events of the PublishMessage: $event_templates"
  fi
  echo "PASS: $1: app-user shows no Emitter event of the PublishMessage (CoreState events through feeCustody: $event_templates)"
}
# publish_and_check <label> <require-guardian4-vote>: publish one message and check it on each host; app-user must show
# no Emitter event of it. With require-guardian4-vote true, also require that the mediator counted a guardian4 approval
# for guardianGovernance. guardian4 is the third of three confirmers at threshold 2, so with every host connected the
# mediator can finalize on the other two before guardian4's vote counts; the vote is required only when app-provider is
# disconnected and guardian4 is one of the two reachable confirmers.
publish_and_check() {
  local label="$1" require_guardian4_vote="$2" guardian4_begin sv_begin started_at transaction update_id approval_lines
  guardian4_begin="$(ledger_end_at "$GUARDIAN4_JSON_API_URL")"
  sv_begin="$(ledger_end_at "$(json_api_url sv)")"
  started_at="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  transaction="$(publish_message "hosting-change-$label-$(date +%s)" "c0ffee$(printf '%02x' "${#label}")")" \
    || fail "$label: PublishMessage did not commit"
  update_id="$(jq -er .updateId <<<"$transaction")"
  echo "PASS: $label: PublishMessage committed, update $update_id"
  scan_confirms_publish "$label" guardian4 "$GUARDIAN4_JSON_API_URL" "$guardian4_begin" "$update_id"
  scan_confirms_publish "$label" sv "$(json_api_url sv)" "$sv_begin" "$update_id"
  check_app_user_has_no_emitter_event "$label" "$update_id"
  if [[ "$require_guardian4_vote" == true ]]; then
    approval_lines="$(docker logs canton --since "$started_at" 2>&1 \
      | jq -rR 'fromjson? | select(.message | test("Received verdict LocalApprove\\(\\) for pending parties .*guardianGovernance")) | .message | .[0:260]')"
    grep -m1 "by participant PAR::guardian4::" <<<"$approval_lines" \
      || fail "$label: the mediator log has no guardian4 approval for guardianGovernance"
    echo "PASS: $label: guardian4 approved for guardianGovernance"
  fi
}

publish_and_check all-hosts-connected false
set_participant_connection disconnect app-provider || fail "cannot disconnect app-provider"
trap 'set_participant_connection reconnect app-provider || echo "ERROR: app-provider stays disconnected; reconnect it with participant_connection.canton"; rm -rf "$gg_work_dir"' EXIT
publish_and_check app-provider-disconnected true
echo "PASS: gg_hosting_change_live_check"
