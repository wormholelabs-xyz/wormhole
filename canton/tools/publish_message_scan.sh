#!/usr/bin/env bash
# Read the updates of a party on one participant and print each wormhole-core PublishMessage exercise, of any package.
# Usage: publish_message_scan.sh <json-api-url> <token-file> <read-as-party> <begin-exclusive-offset>
#          <end-inclusive-offset> <required-update-id>
# Output: one JSON line per PublishMessage event: {offset, recordTime, updateId, executingPackageId, sequence}.
# The token file holds a Ledger API JWT of a user with only CanReadAs(<read-as-party>).
# The JSON Ledger API rejects a request with more matching updates than its node limit (200 on LocalNet). The script
# reads pages of SCAN_PAGE_SIZE updates and continues after the last offset of each full page.
# A response can end before the end offset. The offset of the last update cannot show this, because the party filter
# skips updates. <required-update-id> is a control PublishMessage at the end offset: the scan exits 1 unless its output
# contains that update at exactly the end offset. Each curl call stops after SCAN_CURL_MAX_SECONDS (default 60).
# The script uses only curl and jq, thus a guardian can run it against a production participant.
set -euo pipefail

readonly SCAN_PAGE_SIZE="${SCAN_PAGE_SIZE:-200}"
readonly SCAN_CURL_MAX_SECONDS="${SCAN_CURL_MAX_SECONDS:-60}"
readonly SCAN_CURL_CONNECT_SECONDS=10
readonly EMITTER_TEMPLATE_SUFFIX=":Wormhole.Core.State:Emitter"

if [[ $# -ne 6 ]]; then
  echo "usage: publish_message_scan.sh <json-api-url> <token-file> <read-as-party> <begin-exclusive-offset> <end-inclusive-offset> <required-update-id>" >&2
  exit 2
fi
json_api_url="$1"
token_file="$2"
read_as_party="$3"
begin_exclusive="$4"
end_inclusive="$5"
required_update_id="$6"
# A Ledger API update id is a SHA-256 multihash: 0x12 0x20 and 32 bytes, as 68 lowercase hex characters.
if [[ ! "$required_update_id" =~ ^1220[0-9a-f]{64}$ ]]; then
  echo "required update id is not 1220 and 64 lowercase hex characters: $required_update_id" >&2
  exit 2
fi
[[ -r "$token_file" ]] || { echo "cannot read token file $token_file" >&2; exit 2; }
[[ "$begin_exclusive" =~ ^(0|[1-9][0-9]*)$ ]] || { echo "begin offset is not a non-negative integer: $begin_exclusive" >&2; exit 2; }
[[ "$end_inclusive" =~ ^(0|[1-9][0-9]*)$ ]] || { echo "end offset is not a non-negative integer: $end_inclusive" >&2; exit 2; }
[[ "$begin_exclusive" -lt "$end_inclusive" ]] || { echo "begin offset $begin_exclusive is not before end offset $end_inclusive" >&2; exit 2; }
[[ "$SCAN_PAGE_SIZE" =~ ^[1-9][0-9]*$ ]] || { echo "SCAN_PAGE_SIZE is not a positive integer: $SCAN_PAGE_SIZE" >&2; exit 2; }
[[ "$SCAN_CURL_MAX_SECONDS" =~ ^[1-9][0-9]*$ ]] || { echo "SCAN_CURL_MAX_SECONDS is not a positive integer: $SCAN_CURL_MAX_SECONDS" >&2; exit 2; }

work_dir="$(mktemp -d)"
trap 'rm -rf "$work_dir"' EXIT
readonly auth_header="$work_dir/auth"
(umask 077 && printf 'Authorization: Bearer %s\n' "$(tr -d '[:space:]' < "$token_file")" > "$auth_header")

# read_update_page <begin-exclusive>: one page of updates, as a JSON array.
read_update_page() {
  curl --connect-timeout "$SCAN_CURL_CONNECT_SECONDS" --max-time "$SCAN_CURL_MAX_SECONDS" -sS -X POST "$json_api_url/v2/updates?limit=$SCAN_PAGE_SIZE" -H "@$auth_header" -H 'Content-Type: application/json' \
    -d "$(jq -n --argjson begin "$1" --argjson end "$end_inclusive" --arg party "$read_as_party" '{beginExclusive: $begin, endInclusive: $end,
      updateFormat: {includeTransactions: {
        eventFormat: {filtersByParty: {($party): {cumulative: [{identifierFilter: {WildcardFilter: {value: {includeCreatedEventBlob: false}}}}]}}, verbose: false},
        transactionShape: "TRANSACTION_SHAPE_LEDGER_EFFECTS"}}}')"
}

page_begin="$begin_exclusive"
# Each page advances the begin offset by at least one, thus the loop ends after at most end - begin pages.
touch "$work_dir/scan.jsonl"
while [[ "$page_begin" -lt "$end_inclusive" ]]; do
  curl_status=0
  read_update_page "$page_begin" > "$work_dir/page.json" || curl_status=$?
  [[ "$curl_status" -eq 0 ]] || { echo "update read after offset $page_begin failed with curl status $curl_status" >&2; exit 1; }
  if ! jq -e 'type == "array"' "$work_dir/page.json" > /dev/null; then
    echo "update read after offset $page_begin failed: $(jq -c '{code, cause}' "$work_dir/page.json" 2>/dev/null || cat "$work_dir/page.json")" >&2
    exit 1
  fi
  jq -c --arg suffix "$EMITTER_TEMPLATE_SUFFIX" '.[] | .update.Transaction?.value // empty
    | . as $transaction
    | .events[] | .ExercisedEvent? // empty
    | select(.choice == "PublishMessage" and (.templateId | endswith($suffix)))
    | {offset: $transaction.offset, recordTime: $transaction.recordTime, updateId: $transaction.updateId,
       executingPackageId: (.templateId | split(":")[0]), sequence: (.exerciseResult._2.sequence? // null)}' "$work_dir/page.json" \
    | tee -a "$work_dir/scan.jsonl"
  page_length="$(jq length "$work_dir/page.json")"
  [[ "$page_length" -le "$SCAN_PAGE_SIZE" ]] || { echo "the participant returned $page_length updates for a page of $SCAN_PAGE_SIZE" >&2; exit 1; }
  if [[ "$page_length" -lt "$SCAN_PAGE_SIZE" ]]; then
    break
  fi
  page_last_offset="$(jq -er '.[-1].update | to_entries[0].value.value.offset' "$work_dir/page.json")"
  [[ "$page_last_offset" -gt "$page_begin" && "$page_last_offset" -le "$end_inclusive" ]] \
    || { echo "page offset $page_last_offset is outside ($page_begin, $end_inclusive]" >&2; exit 1; }
  page_begin="$page_last_offset"
done

required_update_offsets="$(jq -sc --arg id "$required_update_id" '[.[] | select(.updateId == $id) | .offset] | unique' "$work_dir/scan.jsonl")"
if [[ "$required_update_offsets" == "[]" ]]; then
  echo "scan ended without the required update $required_update_id; the response may be incomplete" >&2
  exit 1
fi
if [[ "$required_update_offsets" != "[$end_inclusive]" ]]; then
  echo "required update $required_update_id is at offsets $required_update_offsets, not at the end offset $end_inclusive" >&2
  exit 1
fi
