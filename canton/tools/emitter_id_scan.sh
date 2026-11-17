#!/usr/bin/env bash
# Pre-ceremony scan of the EmitterRegistry migration (A14_EMITTER_ID_UNIQUENESS.md, "Migration from 0.7.0", step 5).
# Usage: emitter_id_scan.sh <json-api-url> <token-file> <guardian-governance-party> <operator-party>
#                           <begin-exclusive-offset> [<baseline-max-emitter-id>]
# The token file holds a Ledger API JWT of a user with exactly one right: CanReadAs(<guardian-governance-party>).
# The script reads each update of guardianGovernance in (<begin>, <ledger end>], and the active contracts at <ledger end>.
# A begin offset above 0 needs <baseline-max-emitter-id>: the historyMaxEmitterId of an earlier scan that ended at <begin>.
# Use -1 as the baseline when no Emitter existed at <begin>.
# Output: one JSON summary line on stdout, with firstId = the nextId of the new EmitterRegistry.
# Exit 0: no duplicate. Exit 1: a duplicate (operator, owner, emitterId) among active Emitters, or a check failed.
# EXPECT_ACTIVE_REGISTRIES (default 0) is the number of active EmitterRegistry contracts of the pair that the scan requires.
# The script uses only curl and jq, thus a guardian can run it against a production participant.
set -euo pipefail

readonly SCAN_PAGE_SIZE="${SCAN_PAGE_SIZE:-200}"
readonly EXPECT_ACTIVE_REGISTRIES="${EXPECT_ACTIVE_REGISTRIES:-0}"
readonly EMITTER_TEMPLATE_SUFFIX=":Wormhole.Core.State:Emitter"
readonly REGISTRY_TEMPLATE_SUFFIX=":Wormhole.Core.State:EmitterRegistry"
readonly EMITTER_PACKAGE_NAME_TEMPLATE="#wormhole-core:Wormhole.Core.State:Emitter"
readonly REGISTRY_PACKAGE_NAME_TEMPLATE="#wormhole-core:Wormhole.Core.State:EmitterRegistry"

if [[ $# -lt 5 || $# -gt 6 ]]; then
  echo "usage: emitter_id_scan.sh <json-api-url> <token-file> <guardian-governance-party> <operator-party> <begin-exclusive-offset> [<baseline-max-emitter-id>]" >&2
  exit 2
fi
json_api_url="$1"
token_file="$2"
guardian_governance_party="$3"
operator_party="$4"
begin_exclusive="$5"
[[ -r "$token_file" ]] || { echo "cannot read token file $token_file" >&2; exit 2; }
[[ "$begin_exclusive" =~ ^[0-9]+$ ]] || { echo "begin offset is not a non-negative integer: $begin_exclusive" >&2; exit 2; }
[[ "$SCAN_PAGE_SIZE" =~ ^[1-9][0-9]*$ ]] || { echo "SCAN_PAGE_SIZE is not a positive integer: $SCAN_PAGE_SIZE" >&2; exit 2; }
[[ "$EXPECT_ACTIVE_REGISTRIES" =~ ^[0-9]+$ ]] || { echo "EXPECT_ACTIVE_REGISTRIES is not a non-negative integer" >&2; exit 2; }
if [[ $# -eq 6 ]]; then
  baseline_max_emitter_id="$6"
  [[ "$baseline_max_emitter_id" =~ ^(-1|[0-9]+)$ ]] || { echo "baseline is not -1 or a non-negative integer: $baseline_max_emitter_id" >&2; exit 2; }
elif [[ "$begin_exclusive" -eq 0 ]]; then
  baseline_max_emitter_id="-1"
else
  echo "a begin offset above 0 needs <baseline-max-emitter-id> from a scan that ended at that offset" >&2
  exit 2
fi

work_dir="$(mktemp -d)"
trap 'rm -rf "$work_dir"' EXIT
readonly auth_header="$work_dir/auth"
(umask 077 && printf 'Authorization: Bearer %s\n' "$(tr -d '[:space:]' < "$token_file")" > "$auth_header")

# SECURITY: the scan must not run with a right that can submit. Only CanReadAs(guardianGovernance) is accepted.
user_id="$(curl -sSf "$json_api_url/v2/authenticated-user" -H "@$auth_header" | jq -er .user.id)"
user_rights="$(curl -sSf "$json_api_url/v2/users/$user_id/rights" -H "@$auth_header" \
  | jq -c '[.rights[].kind | to_entries[0] | {right: .key, party: (.value.value.party // null)}] | sort_by(.right, .party)')"
expected_rights="$(jq -nc --arg gg "$guardian_governance_party" '[{right: "CanReadAs", party: $gg}]')"
if [[ "$user_rights" != "$expected_rights" ]]; then
  echo "user $user_id has rights $user_rights; the scan needs exactly $expected_rights" >&2
  exit 1
fi

end_inclusive="$(curl -sSf "$json_api_url/v2/state/ledger-end" -H "@$auth_header" | jq -er .offset)"
[[ "$end_inclusive" =~ ^[0-9]+$ ]] || { echo "ledger end is not a non-negative integer: $end_inclusive" >&2; exit 1; }
[[ "$begin_exclusive" -le "$end_inclusive" ]] || { echo "begin offset $begin_exclusive is after ledger end $end_inclusive" >&2; exit 2; }

# read_update_page <begin-exclusive>: one page of guardianGovernance updates, as a JSON array.
read_update_page() {
  curl -sS -X POST "$json_api_url/v2/updates?limit=$SCAN_PAGE_SIZE" -H "@$auth_header" -H 'Content-Type: application/json' \
    -d "$(jq -n --argjson begin "$1" --argjson end "$end_inclusive" --arg party "$guardian_governance_party" '{
      beginExclusive: $begin, endInclusive: $end,
      updateFormat: {includeTransactions: {
        eventFormat: {filtersByParty: {($party): {cumulative: [{identifierFilter: {WildcardFilter: {value: {includeCreatedEventBlob: false}}}}]}}, verbose: false},
        transactionShape: "TRANSACTION_SHAPE_LEDGER_EFFECTS"}}}')"
}

# Each create of an Emitter or EmitterRegistry of the pair, archived or active, of any package: one JSON line.
: > "$work_dir/history.jsonl"
page_begin="$begin_exclusive"
# Each page advances the begin offset by at least one, thus the loop ends after at most end - begin pages.
while [[ "$page_begin" -lt "$end_inclusive" ]]; do
  read_update_page "$page_begin" > "$work_dir/page.json"
  if ! jq -e 'type == "array"' "$work_dir/page.json" > /dev/null; then
    echo "update read after offset $page_begin failed: $(jq -c '{code, cause}' "$work_dir/page.json" 2>/dev/null || cat "$work_dir/page.json")" >&2
    exit 1
  fi
  jq -c --arg emitterSuffix "$EMITTER_TEMPLATE_SUFFIX" --arg registrySuffix "$REGISTRY_TEMPLATE_SUFFIX" \
    --arg operator "$operator_party" --arg gg "$guardian_governance_party" '.[] | .update.Transaction?.value // empty
    | . as $transaction | .events[] | .CreatedEvent? // empty
    | select(.createArgument.operator == $operator and .createArgument.guardianGovernance == $gg)
    | if (.templateId | endswith($emitterSuffix)) then
        {kind: "Emitter", offset: $transaction.offset, contractId, owner: .createArgument.owner,
         emitterId: (.createArgument.emitterId | tonumber)}
      elif (.templateId | endswith($registrySuffix)) then
        {kind: "EmitterRegistry", offset: $transaction.offset, contractId, nextId: (.createArgument.nextId | tonumber)}
      else empty end' "$work_dir/page.json" >> "$work_dir/history.jsonl"
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

# Active Emitter and EmitterRegistry contracts of all wormhole-core versions, at the same end offset.
curl -sS -X POST "$json_api_url/v2/state/active-contracts" -H "@$auth_header" -H 'Content-Type: application/json' \
  -d "$(jq -n --argjson end "$end_inclusive" --arg party "$guardian_governance_party" \
    --arg emitter "$EMITTER_PACKAGE_NAME_TEMPLATE" --arg registry "$REGISTRY_PACKAGE_NAME_TEMPLATE" '{
    activeAtOffset: $end,
    eventFormat: {filtersByParty: {($party): {cumulative: [
      {identifierFilter: {TemplateFilter: {value: {templateId: $emitter, includeCreatedEventBlob: false}}}},
      {identifierFilter: {TemplateFilter: {value: {templateId: $registry, includeCreatedEventBlob: false}}}}]}},
      verbose: false}}')" > "$work_dir/acs.json"
if ! jq -e 'type == "array"' "$work_dir/acs.json" > /dev/null; then
  echo "active contract read at offset $end_inclusive failed: $(jq -c '{code, cause}' "$work_dir/acs.json" 2>/dev/null || cat "$work_dir/acs.json")" >&2
  exit 1
fi
jq -c --arg operator "$operator_party" --arg gg "$guardian_governance_party" --arg emitterSuffix "$EMITTER_TEMPLATE_SUFFIX" \
  '.[] | .contractEntry.JsActiveContract?.createdEvent // empty
  | select(.createArgument.operator == $operator and .createArgument.guardianGovernance == $gg)
  | if (.templateId | endswith($emitterSuffix)) then
      {kind: "Emitter", contractId, owner: .createArgument.owner, emitterId: (.createArgument.emitterId | tonumber)}
    else
      {kind: "EmitterRegistry", contractId, nextId: (.createArgument.nextId | tonumber)}
    end' "$work_dir/acs.json" > "$work_dir/active.jsonl"

jq -sc --slurpfile active <(jq -sc . "$work_dir/active.jsonl") \
  --argjson baseline "$baseline_max_emitter_id" --argjson begin "$begin_exclusive" --argjson end "$end_inclusive" \
  --argjson expectedRegistries "$EXPECT_ACTIVE_REGISTRIES" --arg operator "$operator_party" --arg gg "$guardian_governance_party" '
  . as $history | $active[0] as $active
  | ($history | map(select(.kind == "Emitter"))) as $historyEmitters
  | ($active | map(select(.kind == "Emitter"))) as $activeEmitters
  | ($active | map(select(.kind == "EmitterRegistry"))) as $activeRegistries
  | ([$baseline] + ($historyEmitters | map(.emitterId)) | max) as $historyMaxEmitterId
  | ([0] + ($history | map(select(.kind == "EmitterRegistry") | .nextId)) | max) as $historyMaxRegistryNextId
  | ($historyEmitters | map(.contractId)) as $historyEmitterIds
  | {
      operator: $operator, guardianGovernance: $gg, beginExclusiveOffset: $begin, endInclusiveOffset: $end,
      baselineMaxEmitterId: $baseline,
      historyEmitterCreates: ($historyEmitters | length),
      historyMaxEmitterId: $historyMaxEmitterId,
      historyMaxRegistryNextId: $historyMaxRegistryNextId,
      activeEmitters: ($activeEmitters | length),
      activeMaxEmitterId: ([-1] + ($activeEmitters | map(.emitterId)) | max),
      activeRegistries: ($activeRegistries | length),
      negativeEmitterIds: ([$historyEmitters[], $activeEmitters[]] | map(select(.emitterId < 0)) | length),
      activeEmittersMissingFromHistory: (if $begin == 0
        then ($activeEmitters | map(select(.contractId as $cid | $historyEmitterIds | index($cid) | not)) | length)
        else null end),
      activeDuplicates: ($activeEmitters | group_by([.owner, .emitterId]) | map(select(length > 1)
        | {owner: .[0].owner, emitterId: .[0].emitterId, contractIds: map(.contractId)})),
      firstId: ([$historyMaxEmitterId + 1, $historyMaxRegistryNextId] | max),
      expectedActiveRegistries: $expectedRegistries
    }' "$work_dir/history.jsonl" > "$work_dir/summary.json"
cat "$work_dir/summary.json"

failures=()
[[ "$(jq '.activeDuplicates | length' "$work_dir/summary.json")" -eq 0 ]] \
  || failures+=("duplicate (operator, owner, emitterId) among active Emitters")
[[ "$(jq '.negativeEmitterIds' "$work_dir/summary.json")" -eq 0 ]] || failures+=("an Emitter has a negative emitterId")
[[ "$(jq '.activeEmittersMissingFromHistory // 0' "$work_dir/summary.json")" -eq 0 ]] \
  || failures+=("an active Emitter is not in the update history from genesis")
[[ "$(jq '.activeMaxEmitterId < .firstId' "$work_dir/summary.json")" == "true" ]] \
  || failures+=("an active Emitter has an emitterId at or above firstId")
[[ "$(jq '.activeRegistries' "$work_dir/summary.json")" -eq "$EXPECT_ACTIVE_REGISTRIES" ]] \
  || failures+=("active EmitterRegistry count is not $EXPECT_ACTIVE_REGISTRIES")
if [[ ${#failures[@]} -gt 0 ]]; then
  printf 'emitter_id_scan: FAIL: %s\n' "${failures[@]}" >&2
  exit 1
fi
echo "emitter_id_scan: PASS: firstId $(jq .firstId "$work_dir/summary.json") at offset $end_inclusive" >&2
