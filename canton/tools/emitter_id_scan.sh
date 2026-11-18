#!/usr/bin/env bash
# Scan of the Emitter and EmitterRegistry history of one (operator, guardianGovernance) pair
# (A14_EMITTER_ID_UNIQUENESS.md, "Scan"; "Deployment of 0.8.0", step 6).
# Usage: emitter_id_scan.sh <json-api-url> <token-file> <guardian-governance-party> <operator-party>
#                           <begin-exclusive-offset> [<baseline-max-emitter-id>]
# The token file holds a Ledger API JWT of a user with exactly one right: CanReadAs(<guardian-governance-party>).
# The script reads each update of guardianGovernance in (<begin>, <ledger end>], and the active contracts at <ledger end>.
# A begin offset above 0 needs <baseline-max-emitter-id>: the historyMaxEmitterId of an earlier scan that ended at <begin>.
# Use -1 as the baseline when no Emitter existed at <begin>.
# Output: one JSON summary line on stdout, with firstId = historyMaxEmitterId + 1, the nextId of the new EmitterRegistry.
# Each active EmitterRegistry must have nextId == firstId; historyMaxRegistryNextId is reported only.
# Exit 0: no duplicate. Exit 1: a duplicate (operator, owner, emitterId) among active Emitters, or a check failed.
# EXPECT_ACTIVE_REGISTRIES (default 0) is the number of active EmitterRegistry contracts of the pair that the scan requires.
# EXPECTED_CORE_PACKAGE_ID (required) is the only wormhole-core package id that this participant may vet.
# CURL_MAX_TIME_SECONDS (default 60) bounds each Ledger API call.
# The script uses only curl and jq, thus a guardian can run it against a production participant.
set -euo pipefail

readonly SCAN_PAGE_SIZE="${SCAN_PAGE_SIZE:-200}"
readonly EXPECT_ACTIVE_REGISTRIES="${EXPECT_ACTIVE_REGISTRIES:-0}"
readonly CURL_MAX_TIME_SECONDS="${CURL_MAX_TIME_SECONDS:-60}"
readonly CURL_CONNECT_TIMEOUT_SECONDS=10
readonly CORE_PACKAGE_NAME="wormhole-core"
# Canton 3.7.3 maximum for /v2/package-vetting/list.
readonly VETTING_PAGE_SIZE=100
readonly EMITTER_TEMPLATE_SUFFIX=":Wormhole.Core.State:Emitter"
readonly REGISTRY_TEMPLATE_SUFFIX=":Wormhole.Core.State:EmitterRegistry"
readonly EMITTER_PACKAGE_NAME_TEMPLATE="#wormhole-core:Wormhole.Core.State:Emitter"
readonly REGISTRY_PACKAGE_NAME_TEMPLATE="#wormhole-core:Wormhole.Core.State:EmitterRegistry"
# Wormhole.Core.State.maxEmitterId (2^53 - 1): every id stays exact in jq doubles.
readonly MAX_EMITTER_ID="9007199254740991"
readonly MAX_REGISTRY_NEXT_ID="9007199254740992"
# exact_integer($maxText): the number for a canonical decimal string in 0..$maxText, else null.
# shellcheck disable=SC2016 # jq source
readonly JQ_EXACT_INTEGER='def exact_integer($maxText):
  if type == "string" and test("^(0|[1-9][0-9]{0,15})$")
     and (length < ($maxText | length) or (length == ($maxText | length) and . <= $maxText))
  then tonumber else null end;'

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
[[ "$CURL_MAX_TIME_SECONDS" =~ ^[1-9][0-9]*$ ]] || { echo "CURL_MAX_TIME_SECONDS is not a positive integer" >&2; exit 2; }
[[ "${EXPECTED_CORE_PACKAGE_ID:-}" =~ ^[0-9a-f]{64}$ ]] \
  || { echo "EXPECTED_CORE_PACKAGE_ID is not set to a 64-character lowercase hex package id" >&2; exit 2; }
if [[ $# -eq 6 ]]; then
  baseline_max_emitter_id="$6"
  # At most 16 digits, thus the comparison is exact in bash 64-bit arithmetic.
  if [[ ! "$baseline_max_emitter_id" =~ ^(-1|0|[1-9][0-9]{0,15})$ ]] || (( baseline_max_emitter_id > MAX_EMITTER_ID )); then
    echo "baseline is not -1 or a canonical integer in 0..$MAX_EMITTER_ID: $baseline_max_emitter_id" >&2
    exit 2
  fi
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

# curl_ledger_api <request-name> <output-file> <curl-argument>...: one Ledger API call with a time bound.
# Exits 1 when curl fails, including a timeout.
curl_ledger_api() {
  local request_name="$1" output_file="$2" curl_exit=0
  shift 2
  curl -sS --max-time "$CURL_MAX_TIME_SECONDS" --connect-timeout "$CURL_CONNECT_TIMEOUT_SECONDS" \
    -H "@$auth_header" -o "$output_file" "$@" || curl_exit=$?
  if [[ "$curl_exit" -ne 0 ]]; then
    echo "Ledger API request $request_name failed (curl exit $curl_exit; 28 is a timeout after ${CURL_MAX_TIME_SECONDS}s)" >&2
    exit 1
  fi
}

# SECURITY: the scan must not run with a right that can submit. Only CanReadAs(guardianGovernance) is accepted.
curl_ledger_api authenticated-user "$work_dir/user.json" -f "$json_api_url/v2/authenticated-user"
user_id="$(jq -er .user.id "$work_dir/user.json")"
curl_ledger_api user-rights "$work_dir/rights.json" -f "$json_api_url/v2/users/$user_id/rights"
user_rights="$(jq -c '[.rights[].kind | to_entries[0] | {right: .key, party: (.value.value.party // null)}] | sort_by(.right, .party)' \
  "$work_dir/rights.json")"
expected_rights="$(jq -nc --arg gg "$guardian_governance_party" '[{right: "CanReadAs", party: $gg}]')"
if [[ "$user_rights" != "$expected_rights" ]]; then
  echo "user $user_id has rights $user_rights; the scan needs exactly $expected_rights" >&2
  exit 1
fi

# SECURITY: an older wormhole-core vetted next to the expected one can still create Emitters outside the registry.
curl_ledger_api participant-id "$work_dir/participant.json" -f "$json_api_url/v2/parties/participant-id"
participant_id="$(jq -er .participantId "$work_dir/participant.json")"
curl_ledger_api package-vetting "$work_dir/vetting.json" -f -X POST "$json_api_url/v2/package-vetting/list" \
  -H 'Content-Type: application/json' \
  -d "$(jq -n --arg packageName "$CORE_PACKAGE_NAME" --arg participant "$participant_id" --argjson pageSize "$VETTING_PAGE_SIZE" '{
    packageMetadataFilter: {packageIds: [], packageNamePrefixes: [$packageName]},
    topologyStateFilter: {participantIds: [$participant], synchronizerIds: []},
    pageToken: "", pageSize: $pageSize}')"
[[ "$(jq -r '.nextPageToken // ""' "$work_dir/vetting.json")" == "" ]] \
  || { echo "the participant vets more than $VETTING_PAGE_SIZE wormhole-core* packages" >&2; exit 1; }
vetted_core_package_ids="$(jq -c --arg packageName "$CORE_PACKAGE_NAME" \
  '[.vettedPackages[].packages[] | select(.packageName == $packageName) | .packageId] | unique' "$work_dir/vetting.json")"

curl_ledger_api ledger-end "$work_dir/ledger-end.json" -f "$json_api_url/v2/state/ledger-end"
end_inclusive="$(jq -er .offset "$work_dir/ledger-end.json")"
[[ "$end_inclusive" =~ ^[0-9]+$ ]] || { echo "ledger end is not a non-negative integer: $end_inclusive" >&2; exit 1; }
[[ "$begin_exclusive" -le "$end_inclusive" ]] || { echo "begin offset $begin_exclusive is after ledger end $end_inclusive" >&2; exit 2; }

# read_update_page <begin-exclusive>: one page of guardianGovernance updates, as a JSON array in page.json.
read_update_page() {
  curl_ledger_api updates "$work_dir/page.json" -X POST "$json_api_url/v2/updates?limit=$SCAN_PAGE_SIZE" \
    -H 'Content-Type: application/json' \
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
  read_update_page "$page_begin"
  if ! jq -e 'type == "array"' "$work_dir/page.json" > /dev/null; then
    echo "update read after offset $page_begin failed: $(jq -c '{code, cause}' "$work_dir/page.json" 2>/dev/null || cat "$work_dir/page.json")" >&2
    exit 1
  fi
  jq -c --arg emitterSuffix "$EMITTER_TEMPLATE_SUFFIX" --arg registrySuffix "$REGISTRY_TEMPLATE_SUFFIX" \
    --arg operator "$operator_party" --arg gg "$guardian_governance_party" --arg corePackageName "$CORE_PACKAGE_NAME" \
    --arg maxEmitterId "$MAX_EMITTER_ID" --arg maxRegistryNextId "$MAX_REGISTRY_NEXT_ID" "$JQ_EXACT_INTEGER"'
    .[] | .update.Transaction?.value // empty
    | . as $transaction | .events[] | .CreatedEvent? // empty
    | select(.packageName == $corePackageName)
    | select(.createArgument.operator == $operator and .createArgument.guardianGovernance == $gg)
    | if (.templateId | endswith($emitterSuffix)) then
        {kind: "Emitter", offset: $transaction.offset, contractId, owner: .createArgument.owner,
         emitterIdText: .createArgument.emitterId, emitterId: (.createArgument.emitterId | exact_integer($maxEmitterId))}
      elif (.templateId | endswith($registrySuffix)) then
        {kind: "EmitterRegistry", offset: $transaction.offset, contractId,
         nextIdText: .createArgument.nextId, nextId: (.createArgument.nextId | exact_integer($maxRegistryNextId))}
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
curl_ledger_api active-contracts "$work_dir/acs.json" -X POST "$json_api_url/v2/state/active-contracts" \
  -H 'Content-Type: application/json' \
  -d "$(jq -n --argjson end "$end_inclusive" --arg party "$guardian_governance_party" \
    --arg emitter "$EMITTER_PACKAGE_NAME_TEMPLATE" --arg registry "$REGISTRY_PACKAGE_NAME_TEMPLATE" '{
    activeAtOffset: $end,
    eventFormat: {filtersByParty: {($party): {cumulative: [
      {identifierFilter: {TemplateFilter: {value: {templateId: $emitter, includeCreatedEventBlob: false}}}},
      {identifierFilter: {TemplateFilter: {value: {templateId: $registry, includeCreatedEventBlob: false}}}}]}},
      verbose: false}}')"
if ! jq -e 'type == "array"' "$work_dir/acs.json" > /dev/null; then
  echo "active contract read at offset $end_inclusive failed: $(jq -c '{code, cause}' "$work_dir/acs.json" 2>/dev/null || cat "$work_dir/acs.json")" >&2
  exit 1
fi
jq -c --arg operator "$operator_party" --arg gg "$guardian_governance_party" --arg emitterSuffix "$EMITTER_TEMPLATE_SUFFIX" \
  --arg corePackageName "$CORE_PACKAGE_NAME" \
  --arg maxEmitterId "$MAX_EMITTER_ID" --arg maxRegistryNextId "$MAX_REGISTRY_NEXT_ID" "$JQ_EXACT_INTEGER"'
  .[] | .contractEntry.JsActiveContract?.createdEvent // empty
  | select(.packageName == $corePackageName)
  | select(.createArgument.operator == $operator and .createArgument.guardianGovernance == $gg)
  | if (.templateId | endswith($emitterSuffix)) then
      {kind: "Emitter", contractId, owner: .createArgument.owner,
       emitterIdText: .createArgument.emitterId, emitterId: (.createArgument.emitterId | exact_integer($maxEmitterId))}
    else
      {kind: "EmitterRegistry", contractId,
       nextIdText: .createArgument.nextId, nextId: (.createArgument.nextId | exact_integer($maxRegistryNextId))}
    end' "$work_dir/acs.json" > "$work_dir/active.jsonl"

jq -sc --slurpfile active <(jq -sc . "$work_dir/active.jsonl") \
  --argjson baseline "$baseline_max_emitter_id" --argjson begin "$begin_exclusive" --argjson end "$end_inclusive" \
  --argjson expectedRegistries "$EXPECT_ACTIVE_REGISTRIES" --arg operator "$operator_party" --arg gg "$guardian_governance_party" \
  --arg expectedCorePackageId "$EXPECTED_CORE_PACKAGE_ID" --argjson vettedCorePackageIds "$vetted_core_package_ids" '
  . as $history | $active[0] as $active
  | ($history | map(select(.kind == "Emitter"))) as $historyEmitters
  | ($active | map(select(.kind == "Emitter"))) as $activeEmitters
  | ($active | map(select(.kind == "EmitterRegistry"))) as $activeRegistries
  | ([$baseline] + ($historyEmitters | map(.emitterId | values)) | max) as $historyMaxEmitterId
  | ([0] + ($history | map(select(.kind == "EmitterRegistry") | .nextId | values)) | max) as $historyMaxRegistryNextId
  | ($historyEmitters | map(.contractId)) as $historyEmitterIds
  | {
      operator: $operator, guardianGovernance: $gg, beginExclusiveOffset: $begin, endInclusiveOffset: $end,
      baselineMaxEmitterId: $baseline,
      historyEmitterCreates: ($historyEmitters | length),
      historyMaxEmitterId: $historyMaxEmitterId,
      historyMaxRegistryNextId: $historyMaxRegistryNextId,
      activeEmitters: ($activeEmitters | length),
      activeMaxEmitterId: ([-1] + ($activeEmitters | map(.emitterId | values)) | max),
      activeRegistries: ($activeRegistries | length),
      invalidEmitterIds: ([$historyEmitters[], $activeEmitters[]] | map(select(.emitterId == null))
        | map({contractId, emitterId: .emitterIdText})),
      invalidRegistryNextIds: ([($history | map(select(.kind == "EmitterRegistry")))[], $activeRegistries[]]
        | map(select(.nextId == null)) | map({contractId, nextId: .nextIdText})),
      activeEmittersMissingFromHistory: (if $begin == 0
        then ($activeEmitters | map(select(.contractId as $cid | $historyEmitterIds | index($cid) | not)) | length)
        else null end),
      activeDuplicates: ($activeEmitters | group_by([.owner, .emitterId]) | map(select(length > 1)
        | {owner: .[0].owner, emitterId: .[0].emitterId, contractIds: map(.contractId)})),
      activeRegistriesWithWrongNextId: ($activeRegistries | map(select(.nextId != ($historyMaxEmitterId + 1)))
        | map({contractId, nextId: .nextIdText})),
      firstId: ($historyMaxEmitterId + 1),
      expectedActiveRegistries: $expectedRegistries,
      expectedCorePackageId: $expectedCorePackageId,
      unexpectedVettedCorePackageIds: ($vettedCorePackageIds - [$expectedCorePackageId]),
      expectedCorePackageVetted: ($vettedCorePackageIds | index($expectedCorePackageId) != null)
    }' "$work_dir/history.jsonl" > "$work_dir/summary.json"
cat "$work_dir/summary.json"

failures=()
[[ "$(jq '.activeDuplicates | length' "$work_dir/summary.json")" -eq 0 ]] \
  || failures+=("duplicate (operator, owner, emitterId) among active Emitters")
[[ "$(jq '.invalidEmitterIds | length' "$work_dir/summary.json")" -eq 0 ]] \
  || failures+=("an emitterId is outside 0..maxEmitterId ($MAX_EMITTER_ID) or not canonical: $(jq -c .invalidEmitterIds "$work_dir/summary.json")")
[[ "$(jq '.invalidRegistryNextIds | length' "$work_dir/summary.json")" -eq 0 ]] \
  || failures+=("a registry nextId is outside 0..$MAX_REGISTRY_NEXT_ID or not canonical: $(jq -c .invalidRegistryNextIds "$work_dir/summary.json")")
[[ "$(jq '.activeEmittersMissingFromHistory // 0' "$work_dir/summary.json")" -eq 0 ]] \
  || failures+=("an active Emitter is not in the update history from genesis")
[[ "$(jq '.activeMaxEmitterId < .firstId' "$work_dir/summary.json")" == "true" ]] \
  || failures+=("an active Emitter has an emitterId at or above firstId")
[[ "$(jq '.activeRegistries' "$work_dir/summary.json")" -eq "$EXPECT_ACTIVE_REGISTRIES" ]] \
  || failures+=("active EmitterRegistry count is not $EXPECT_ACTIVE_REGISTRIES")
[[ "$(jq '.activeRegistriesWithWrongNextId | length' "$work_dir/summary.json")" -eq 0 ]] \
  || failures+=("an active EmitterRegistry has nextId other than historyMaxEmitterId + 1")
[[ "$(jq '.unexpectedVettedCorePackageIds | length' "$work_dir/summary.json")" -eq 0 ]] \
  || failures+=("unexpected vetted wormhole-core package ids: $(jq -c .unexpectedVettedCorePackageIds "$work_dir/summary.json")")
[[ "$(jq '.expectedCorePackageVetted' "$work_dir/summary.json")" == "true" ]] \
  || failures+=("the expected wormhole-core package $EXPECTED_CORE_PACKAGE_ID is not vetted")
if [[ ${#failures[@]} -gt 0 ]]; then
  printf 'emitter_id_scan: FAIL: %s\n' "${failures[@]}" >&2
  exit 1
fi
echo "emitter_id_scan: PASS: firstId $(jq .firstId "$work_dir/summary.json") at offset $end_inclusive" >&2
