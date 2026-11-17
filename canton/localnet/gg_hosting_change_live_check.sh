#!/usr/bin/env bash
# Live checks of the guardian replacement rehearsal (L1): a PublishMessage commits with guardian4 as a confirmer, also
# with sv disconnected, and app-user (removed host) no longer receives guardianGovernance transactions.
# Usage: gg_hosting_change_live_check.sh
set -euo pipefail
# shellcheck source=gg_lib.sh
source "$(dirname "${BASH_SOURCE[0]}")/gg_lib.sh"

readonly PUBLISH_MESSAGE_SCAN="$REPO_ROOT/canton/tools/publish_message_scan.sh"
readonly GUARDIAN4_JSON_API_URL="http://localhost:5975"

ledger_end_at() {
  curl_bounded -sSf "$1/v2/state/ledger-end" -H "@$SCRIPT_AUTH_HEADER" | jq -er .offset
}
# publish_and_check <label>: publish one message, then require that guardian4 and app-provider show it and that the
# mediator received a guardian4 approval for guardianGovernance.
publish_and_check() {
  local label="$1" guardian4_begin provider_begin user_begin started_at transaction update_id scan_output
  guardian4_begin="$(ledger_end_at "$GUARDIAN4_JSON_API_URL")"
  provider_begin="$(ledger_end_at "$(json_api_url app-provider)")"
  user_begin="$(ledger_end_at "$(json_api_url app-user)")"
  started_at="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  transaction="$(publish_message "hosting-change-$label-$(date +%s)" "c0ffee$(printf '%02x' "${#label}")")" \
    || fail "$label: PublishMessage did not commit"
  update_id="$(jq -er .updateId <<<"$transaction")"
  echo "PASS: $label: PublishMessage committed, update $update_id"
  scan_output="$("$PUBLISH_MESSAGE_SCAN" "$GUARDIAN4_JSON_API_URL" "$gg_work_dir/script.jwt" "$GG_PARTY" "$guardian4_begin")"
  grep -q "$update_id" <<<"$scan_output" || fail "$label: guardian4 does not show update $update_id as guardianGovernance"
  echo "PASS: $label: guardian4 shows the PublishMessage as guardianGovernance"
  scan_output="$("$PUBLISH_MESSAGE_SCAN" "$(json_api_url app-provider)" "$gg_work_dir/script.jwt" "$GG_PARTY" "$provider_begin")"
  grep -q "$update_id" <<<"$scan_output" || fail "$label: app-provider does not show update $update_id"
  scan_output="$("$PUBLISH_MESSAGE_SCAN" "$(json_api_url app-user)" "$gg_work_dir/script.jwt" "$GG_PARTY" "$user_begin" 2>&1)" || true
  if grep -q "$update_id" <<<"$scan_output"; then
    fail "$label: app-user still receives guardianGovernance transactions"
  fi
  echo "PASS: $label: app-user does not show the PublishMessage"
  docker logs canton --since "$started_at" 2>&1 \
    | jq -rR 'fromjson? | select(.message | test("Received verdict LocalApprove\\(\\) for pending parties .*guardianGovernance")) | .message | .[0:260]' \
    | grep "by participant PAR::guardian4::" | head -n 1 | grep . \
    || fail "$label: the mediator log has no guardian4 approval for guardianGovernance"
  echo "PASS: $label: guardian4 approved for guardianGovernance"
}

publish_and_check all-hosts-connected
set_participant_connection disconnect sv || fail "cannot disconnect sv"
trap 'set_participant_connection reconnect sv || echo "ERROR: sv stays disconnected; reconnect it with participant_connection.canton"; rm -rf "$gg_work_dir"' EXIT
publish_and_check sv-disconnected
echo "PASS: gg_hosting_change_live_check"
