#!/usr/bin/env bash
# Rehearse the wormhole-core 0.6.0 to 0.7.0 upgrade (canton/README.md section 7.5) on LocalNet, in the
# procedure order: dependents first, unvet the old versions immediately before the watcher switch.
# Steps (run one, or "all" in order):
#   prepare       Start state: core 0.6.0 and ntt 0.2.0 vetted, core 0.7.0 and ntt 0.3.0 not vetted, on each participant.
#   observe-v6    Start the guardian watcher v6, pinned to 0.6.0. Publish under 0.6.0. Expect an observation.
#   vet-v7        Record the upgrade start offset. Upload and vet core 0.7.0, then ntt 0.3.0, on each participant.
#   publish-v7    Start the canary watcher, pinned to 0.7.0. Publish under 0.7.0 and under 0.6.0 in one window.
#                 Expect each watcher to observe one message, and to drop and count the other.
#   ntt           ntt_e2e.sh with ntt 0.3.0. Expect each NTT PublishMessage to run under 0.7.0 on each host.
#   gate          Stop the canary. Expect its counter to show the one 0.6.0 publish of publish-v7 and no other.
#   unvet-v6      With v6 still pinned to 0.6.0: unvet core 0.6.0 and ntt 0.2.0 together on each participant.
#                 Expect 0.6.0 and ntt 0.2.0 submissions to fail at prepare. Publish under 0.7.0; v6 drops it.
#   switch-v7     Stop v6 (the coordinated restart). Enumerate the dropped update ids from the v6 WARN logs and
#                 from a scan since the start offset; expect equal sets. Start v7, pinned to 0.7.0, with those
#                 reobservation requests. Expect each dropped update and one live 0.7.0 publish.
#   enforce-v7    A nonce above uint32: 0.7.0 rejects it. 0.6.0 commits it while vetted, and fails after the unvet.
#   post-unvet    Contracts that 0.6.0 and ntt 0.2.0 created work under 0.7.0 and ntt 0.3.0; ntt_e2e.sh passes.
# Usage: core_upgrade_rehearsal.sh <step>|all
# A step that fails stops each watcher harness that it started. A step that passes leaves v6 (observe-v6) and the
# canary (publish-v7) running for the next steps.
# Precondition: gg_publish_and_govern.sh ran; the CoreState message fee is 0; the 0.7.0 core DAR and the ntt 0.3.0
# DAR are built; LocalNet has the ntt 0.2.0 package 8b9c3ee1 and a burn/mint NttManager that it created.
set -euo pipefail
# shellcheck source=ntt_lib.sh
source "$(dirname "${BASH_SOURCE[0]}")/ntt_lib.sh"

readonly CORE_V6_PACKAGE_ID="701a57c867e02d962f231bd9f0ff5186e16462a77289aa1b7aaba7fe95074c15"
readonly CORE_V7_PACKAGE_ID="7a22a1c4026b2a037b021063c4ff2bd5506be2e748aa9de8b7a6057c90c356eb"
# ntt 0.2.0 calls core 0.6.0 by package id. ntt 0.3.0 calls core 0.7.0 and is an upgrade of ntt 0.2.0.
readonly NTT_V2_PACKAGE_ID="8b9c3ee1c93b7039b39421b790c283f507c2b6a01171aa839873f220dc5ff84a"
readonly NTT_V3_PACKAGE_ID="291fcf920b971d50956845c2690723fb321f0a5b141a44caa5a16910f6ffa1a6"
readonly CORE_V6_DAR="$REPO_ROOT/canton/dars/wormhole-core-0.6.0.dar"
readonly CORE_V7_DAR="$REPO_ROOT/canton/core/.daml/dist/wormhole-core-0.7.0.dar"
readonly DPM_BIN_DIR="${DPM_BIN_DIR:-$HOME/.dpm/bin}"
# Each LocalNet participant hosts guardianGovernance. Thus each participant is a guardian host and an informee host.
readonly GUARDIAN_HOSTS=(app-provider sv app-user)
readonly UPGRADE_STATE="$LOCALNET_STATE_DIR/core-upgrade.json"
readonly WATCHER_DIR="$LOCALNET_STATE_DIR/core-upgrade-watcher"
readonly OBSERVATION_WAIT_SECONDS=60
# Time that a watcher gets to show that it drops a message.
readonly DROP_WINDOW_SECONDS=15
readonly NONCE_ABOVE_UINT32=4294967296
readonly SCAN_UPDATE_LIMIT=1000
readonly UNVETTED_ERROR_CODE="INVALID_PRESCRIBED_SYNCHRONIZER_ID"
# Current guardian set of the LocalNet CoreState (after gg_guardian_set_rotation.sh 1 2).
readonly CURRENT_GUARDIAN_SET_INDEX="${CURRENT_GUARDIAN_SET_INDEX:-2}"
readonly CURRENT_GUARDIAN_KEY_INDICES="${CURRENT_GUARDIAN_KEY_INDICES:-2,3,4}"
# shellcheck source=watcher_harness_lib.sh
source "$LOCALNET_SCRIPT_DIR/watcher_harness_lib.sh"
mkdir -p "$WATCHER_DIR"
[[ -f "$UPGRADE_STATE" ]] || echo '{}' > "$UPGRADE_STATE"

fail() {
  echo "FAIL: $*" >&2
  exit 1
}

record_state() {
  local updated
  updated="$(jq --arg key "$1" --arg value "$2" '.[$key] = $value' "$UPGRADE_STATE")"
  printf '%s\n' "$updated" > "$UPGRADE_STATE"
}

append_state_list() {
  local updated
  updated="$(jq --arg key "$1" --arg value "$2" '.[$key] = ((.[$key] // []) + [$value] | unique)' "$UPGRADE_STATE")"
  printf '%s\n' "$updated" > "$UPGRADE_STATE"
}

read_state() {
  jq -er --arg key "$1" '.[$key]' "$UPGRADE_STATE"
}

dar_main_package_id() {
  PATH="$DPM_BIN_DIR:$PATH" dpm damlc inspect-dar --json "$1" 2>/dev/null | jq -er .main_package_id
}

check_dar_package_ids() {
  [[ "$(dar_main_package_id "$CORE_V6_DAR")" == "$CORE_V6_PACKAGE_ID" ]] || fail "$CORE_V6_DAR is not $CORE_V6_PACKAGE_ID"
  [[ -f "$CORE_V7_DAR" ]] || fail "$CORE_V7_DAR not found; run 'dpm build --all' in canton/"
  [[ "$(dar_main_package_id "$CORE_V7_DAR")" == "$CORE_V7_PACKAGE_ID" ]] || fail "$CORE_V7_DAR is not $CORE_V7_PACKAGE_ID"
}

participant_id_of() {
  curl -sSf "$(json_api_url "$1")/v2/parties/participant-id" -H "@$ADMIN_AUTH_HEADER" | jq -er .participantId
}

ledger_end_of() {
  curl -sSf "$(json_api_url "$1")/v2/state/ledger-end" -H "@$SCRIPT_AUTH_HEADER" | jq -er .offset
}

# vetted_versions <validator> <package-name>: "<version> <package id>" of each version that the participant of
# that validator vets, as a sorted JSON array.
vetted_versions() {
  curl -sSf -X POST "$(json_api_url "$1")/v2/package-vetting/list" -H "@$ADMIN_AUTH_HEADER" \
    -H 'Content-Type: application/json' \
    -d "$(jq -n --arg pid "$(participant_id_of "$1")" --arg name "$2" \
      '{packageMetadataFilter: {packageNamePrefixes: [$name]}, topologyStateFilter: {participantIds: [$pid]}}')" \
    | jq -c --arg name "$2" '[.vettedPackages[]?.packages[] | select(.packageName == $name) | "\(.packageVersion) \(.packageId)"] | unique'
}

package_is_vetted() {
  [[ "$(vetted_versions "$1" "$2")" == *" $3\""* ]]
}

# await_vetting <validator> <package-name> <expected-json>
await_vetting() {
  local validator_name="$1" package_name="$2" expected_versions="$3" attempt
  for attempt in $(seq 1 60); do
    if [[ "$attempt" -gt 1 ]]; then sleep 1; fi
    if [[ "$(vetted_versions "$validator_name" "$package_name")" == "$expected_versions" ]]; then
      echo "  $validator_name vets $package_name $(jq -c 'map(.[0:18])' <<<"$expected_versions")"
      return 0
    fi
  done
  fail "$validator_name vets $package_name $(vetted_versions "$validator_name" "$package_name"), expected $expected_versions"
}

versions_json() {
  jq -cn '$ARGS.positional | sort' --args "$@"
}

# upload_and_vet <validator> <dar>: the participant checks upgrade compatibility with each vetted version on upload.
upload_and_vet() {
  local validator_name="$1" dar_path="$2" http_status
  http_status="$(curl -sS -o "$gg_work_dir/upload.out" -w '%{http_code}' -X POST \
    "$(json_api_url "$validator_name")/v2/dars?vetAllPackages=true" \
    -H "@$ADMIN_AUTH_HEADER" -H 'Content-Type: application/octet-stream' --data-binary "@$dar_path")"
  [[ "$http_status" == "200" ]] || fail "upload of $(basename "$dar_path") to $validator_name: HTTP $http_status $(cat "$gg_work_dir/upload.out")"
  echo "  $validator_name: uploaded and vetted $(basename "$dar_path") (HTTP 200)"
}

# change_vetting <validator> <Vet|Unvet> <package-id>...: one topology transaction for all named packages.
change_vetting() {
  local validator_name="$1" operation="$2"
  shift 2
  curl -sSf -X POST "$(json_api_url "$validator_name")/v2/package-vetting/update" -H "@$ADMIN_AUTH_HEADER" \
    -H 'Content-Type: application/json' \
    -d "$(jq -n --arg operation "$operation" --arg sync "$LOGICAL_SYNCHRONIZER_ID" '{
      changes: [{operation: {($operation): {value: {packages: [$ARGS.positional[] | {packageId: .}]}}}}],
      dryRun: false, synchronizerId: $sync}' --args "$@")" \
    > /dev/null
  echo "  $validator_name: $operation $(printf '%.12s ' "$@")in one topology transaction"
}

hex_of() {
  printf '%s' "$1" | od -An -tx1 | tr -d ' \n'
}

# publish_on_operator_emitter <command-id> <template-id> <nonce> <payload-hex>: prints the transaction JSON.
# A template id with a package id selects that package. A "#wormhole-core" template id selects by package name.
publish_on_operator_emitter() {
  submit_interactive "$1" "$(jq -n --arg op "$OPERATOR_PARTY" '[$op]')" \
    "$(exercise_command "$2" "$(core_contract_id emitter)" PublishMessage \
      "$(jq -n --arg op "$OPERATOR_PARTY" --arg cs "$(core_contract_id coreState)" --arg nonce "$3" --arg payload "$4" \
        '{nonce: $nonce, payload: $payload, consistencyLevel: "0", coreStateCid: $cs, payer: $op, feeAllocation: null}')")" \
    "$(operator_signers_json)"
}

# operator_core_contract <Emitter|CoreState>: the created event of the one active contract that Operator uses.
operator_core_contract() {
  active_contracts "$SUBMITTING_VALIDATOR" "$OPERATOR_PARTY" ":Wormhole.Core.State:$1" \
    | jq -ec --arg op "$OPERATOR_PARTY" --arg name "$1" \
      '[.[] | select($name != "Emitter" or .createArgument.owner == $op)]
       | if length == 1 then .[0] else error("expected one active \($name), found \(length)") end'
}

# check_input_contract_created_under <Emitter|CoreState> <package-id>. Each PublishMessage re-creates the Operator
# Emitter under the package that executes it. The CoreState stays the contract that 0.6.0 created.
check_input_contract_created_under() {
  local created_package
  created_package="$(operator_core_contract "$1" | jq -r '.templateId | split(":")[0]')"
  [[ "$created_package" == "$2" ]] || fail "the active $1 was created under ${created_package:0:12}, expected ${2:0:12}"
  echo "  input $1 created under ${created_package:0:12}"
}

# publish_summary <transaction-json>: update id, executing package of PublishMessage, sequence, nonce.
publish_summary() {
  jq -ec '[.events[] | .ExercisedEvent? // empty | select(.choice == "PublishMessage")]
    | if length != 1 then error("expected one PublishMessage event") else .[0] end
    | {executingPackageId: (.templateId | split(":")[0]), sequence: .exerciseResult._2.sequence,
       nonce: .exerciseResult._2.nonce, payload: .exerciseResult._2.payload}' <<<"$1" \
    | jq -c --arg updateId "$(jq -r .updateId <<<"$1")" '. + {updateId: $updateId}'
}

# publish_and_check <label> <template-id> <expected-executing-package-id>: publish on the Operator Emitter, check
# the executing package, and print the summary JSON.
publish_and_check() {
  local label="$1" payload_hex transaction summary
  payload_hex="$(hex_of "core upgrade $(date +%s) $label")"
  transaction="$(publish_on_operator_emitter "core-upgrade-$label-$(date +%s)" "$2" 1 "$payload_hex")"
  summary="$(publish_summary "$transaction")"
  [[ "$(jq -r .executingPackageId <<<"$summary")" == "$3" ]] || fail "$label: PublishMessage did not execute under ${3:0:12}: $summary"
  echo "  published $label: $(jq -c '{executingPackageId: .executingPackageId[0:12], sequence, updateId}' <<<"$summary")" >&2
  echo "$summary"
}

# publish_package_on <validator> <update-id>: package id of the PublishMessage event and the participant offset, as guardianGovernance sees them there.
publish_package_on() {
  curl -sSf -X POST "$(json_api_url "$1")/v2/updates/update-by-id" -H "@$SCRIPT_AUTH_HEADER" \
    -H 'Content-Type: application/json' \
    -d "$(jq -n --arg id "$2" --arg gg "$GG_PARTY" '{updateId: $id, updateFormat: {includeTransactions: {
      eventFormat: {filtersByParty: {($gg): {cumulative: [{identifierFilter: {WildcardFilter: {value: {includeCreatedEventBlob: false}}}}]}}, verbose: false},
      transactionShape: "TRANSACTION_SHAPE_LEDGER_EFFECTS"}}}')" \
    | jq -er '.update.Transaction.value
      | ([.events[] | .ExercisedEvent? // empty | select(.choice == "PublishMessage") | .templateId | split(":")[0]] | unique) as $packages
      | if ($packages | length) != 1 then error("expected one PublishMessage package, found \($packages)")
        else "\($packages[0]) (participant offset \(.offset))" end'
}

# check_publish_package_on_each_host <update-id> <package-id>
check_publish_package_on_each_host() {
  local validator_name event_package
  for validator_name in "${GUARDIAN_HOSTS[@]}"; do
    event_package="$(publish_package_on "$validator_name" "$1")"
    [[ "$event_package" == "$2 "* ]] || fail "$validator_name reports PublishMessage package $event_package for $1"
  done
  echo "  each host reports PublishMessage package ${2:0:12} for $1"
}

# scan_publish_update_ids <begin-exclusive-offset> <end-inclusive-offset> <executing-package-id>: sorted update ids
# that carry a PublishMessage event of that package, as the watcher user reads them for guardianGovernance.
scan_publish_update_ids() {
  local scan_header="$gg_work_dir/watcher-scan.auth" updates
  write_auth_header_file "$scan_header" "$WATCHER_USER_ID"
  updates="$(curl -sSf -X POST "$(json_api_url "$SUBMITTING_VALIDATOR")/v2/updates?limit=$SCAN_UPDATE_LIMIT" -H "@$scan_header" \
    -H 'Content-Type: application/json' \
    -d "$(jq -n --argjson begin "$1" --argjson end "$2" --arg gg "$GG_PARTY" '{beginExclusive: $begin, endInclusive: $end,
      updateFormat: {includeTransactions: {
        eventFormat: {filtersByParty: {($gg): {cumulative: [{identifierFilter: {WildcardFilter: {value: {includeCreatedEventBlob: false}}}}]}}, verbose: false},
        transactionShape: "TRANSACTION_SHAPE_LEDGER_EFFECTS"}}}')")"
  [[ "$(jq length <<<"$updates")" -lt "$SCAN_UPDATE_LIMIT" ]] || fail "the scan reached $SCAN_UPDATE_LIMIT updates; scan a shorter offset range"
  jq -r --arg package "$3" '.[] | .update.Transaction?.value // empty
    | select(any(.events[]; (.ExercisedEvent? // {}) | .choice == "PublishMessage" and (.templateId | split(":")[0]) == $package))
    | .updateId' <<<"$updates" | sort -u
}

# await_observation <harness-name> <sequence> <payload-hex> <reobservation true|false>
await_observation() {
  local observations_file="$WATCHER_DIR/$1.jsonl" sequence="$2" payload_hex="$3" reobserved="$4" waited_seconds
  for ((waited_seconds = 0; waited_seconds < OBSERVATION_WAIT_SECONDS; waited_seconds++)); do
    if [[ -s "$observations_file" ]] && jq -se --argjson seq "$sequence" --arg payload "$payload_hex" --argjson reobserved "$reobserved" \
        '[.[] | select(.sequence == $seq and .payloadHex == $payload and .isReobservation == $reobserved)] | length == 1' \
        "$observations_file" > /dev/null; then
      echo "  watcher $1 observed: $(jq -c --argjson seq "$sequence" --arg payload "$payload_hex" \
        'select(.sequence == $seq and .payloadHex == $payload) | {sequence, nonce, updateId, isReobservation}' "$observations_file")"
      return 0
    fi
    sleep 1
  done
  fail "watcher $1 has no observation of sequence $sequence (reobservation $reobserved) after ${OBSERVATION_WAIT_SECONDS}s"
}

# check_no_observation <harness-name> <update-id>
check_no_observation() {
  if [[ -s "$WATCHER_DIR/$1.jsonl" ]] && jq -se --arg id "$2" 'any(.[]; .updateId == $id)' "$WATCHER_DIR/$1.jsonl" > /dev/null; then
    fail "watcher $1 observed update $2"
  fi
  echo "  PASS: watcher $1 has no observation of update $2"
}

# check_drop_logged <harness-name> <update-id> <executing-package-id>
check_drop_logged() {
  harness_dropped_update_ids "$1" "$3" | grep -qxF "$2" || fail "watcher $1 has no WARN log for the drop of $2 (${3:0:12})"
  echo "  PASS: watcher $1 WARN log: dropped $2, executing package ${3:0:12}"
}

harness_is_running() {
  [[ -f "$WATCHER_DIR/$1.pid" ]] && harness_pid_is_harness "$(cat "$WATCHER_DIR/$1.pid")"
}

# expect_unvetted_rejection <output-prefix> <package-id> <description>: the error has the error code and names each
# participant with "has not vetted <package id prefix>" (the error abbreviates package ids to 12 characters).
expect_unvetted_rejection() {
  local error_file="$1.err" package_id="$2" description="$3" naming_count
  grep -qF "$UNVETTED_ERROR_CODE" "$error_file" || { cat "$error_file" >&2; fail "$description: no $UNVETTED_ERROR_CODE"; }
  naming_count="$(grep -oF "has not vetted ${package_id:0:12}" "$error_file" | wc -l | tr -d ' ')"
  [[ "$naming_count" -eq "${#GUARDIAN_HOSTS[@]}" ]] \
    || { cat "$error_file" >&2; fail "$description: 'has not vetted ${package_id:0:12}' names $naming_count participants, expected ${#GUARDIAN_HOSTS[@]}"; }
  echo "  PASS: $description rejected ($UNVETTED_ERROR_CODE): each of the ${#GUARDIAN_HOSTS[@]} participants 'has not vetted ${package_id:0:12}'"
}

# expect_daml_failure <output-prefix> <assertion-message> <description>
expect_daml_failure() {
  grep -qF "DAML_FAILURE" "$1.err" || { cat "$1.err" >&2; fail "$3: no DAML_FAILURE"; }
  grep -qF "AssertionFailed (error category 9): $2" "$1.err" || { cat "$1.err" >&2; fail "$3: no assertion '$2'"; }
  echo "  PASS: $3 rejected (DAML_FAILURE): AssertionFailed: $2"
}

step_prepare() {
  echo "### prepare: core 0.6.0 and ntt 0.2.0 vetted; core 0.7.0 and ntt 0.3.0 not vetted; on each participant"
  check_dar_package_ids
  local validator_name newer_packages
  for validator_name in "${GUARDIAN_HOSTS[@]}"; do
    newer_packages=()
    package_is_vetted "$validator_name" ntt "$NTT_V3_PACKAGE_ID" && newer_packages+=("$NTT_V3_PACKAGE_ID")
    package_is_vetted "$validator_name" wormhole-core "$CORE_V7_PACKAGE_ID" && newer_packages+=("$CORE_V7_PACKAGE_ID")
    if [[ "${#newer_packages[@]}" -gt 0 ]]; then
      change_vetting "$validator_name" Unvet "${newer_packages[@]}"
    fi
    upload_and_vet "$validator_name" "$CORE_V6_DAR"
    package_is_vetted "$validator_name" ntt "$NTT_V2_PACKAGE_ID" || change_vetting "$validator_name" Vet "$NTT_V2_PACKAGE_ID"
  done
  for validator_name in "${GUARDIAN_HOSTS[@]}"; do
    await_vetting "$validator_name" wormhole-core "$(versions_json "0.6.0 $CORE_V6_PACKAGE_ID")"
    await_vetting "$validator_name" ntt "$(versions_json "0.2.0 $NTT_V2_PACKAGE_ID")"
  done
  echo '{}' > "$UPGRADE_STATE"
}

step_observe_v6() {
  echo "### 1. Guardian watcher v6, pinned to 0.6.0, observes a 0.6.0 PublishMessage"
  build_harness
  if harness_is_running v6; then stop_harness v6; fi
  run_harness v6 "$CORE_V6_PACKAGE_ID" "" 3600
  check_input_contract_created_under CoreState "$CORE_V6_PACKAGE_ID"
  local summary
  summary="$(publish_and_check v6-baseline "$CORE_V6_PACKAGE_ID:Wormhole.Core.State:Emitter" "$CORE_V6_PACKAGE_ID")"
  await_observation v6 "$(jq -r .sequence <<<"$summary")" "$(jq -r .payload <<<"$summary")" false
}

step_vet_v7() {
  echo "### 2. Record the upgrade start offset. Vet core 0.7.0, then ntt 0.3.0, on each participant"
  check_dar_package_ids
  local validator_name start_offset
  start_offset="$(ledger_end_of "$SUBMITTING_VALIDATOR")"
  record_state upgradeStartOffset "$start_offset"
  echo "  upgrade start offset on $SUBMITTING_VALIDATOR: $start_offset"
  for validator_name in "${GUARDIAN_HOSTS[@]}"; do
    upload_and_vet "$validator_name" "$CORE_V7_DAR"
  done
  for validator_name in "${GUARDIAN_HOSTS[@]}"; do
    await_vetting "$validator_name" wormhole-core "$(versions_json "0.6.0 $CORE_V6_PACKAGE_ID" "0.7.0 $CORE_V7_PACKAGE_ID")"
  done
  "$LOCALNET_SCRIPT_DIR/ntt_upload_dars.sh"
  for validator_name in "${GUARDIAN_HOSTS[@]}"; do
    await_vetting "$validator_name" ntt "$(versions_json "0.2.0 $NTT_V2_PACKAGE_ID" "0.3.0 $NTT_V3_PACKAGE_ID")"
  done
}

step_publish_v7() {
  echo "### 3. Canary watcher pinned to 0.7.0. One 0.7.0 and one 0.6.0 publish on the 0.6.0 Emitter and CoreState"
  harness_is_running v6 || fail "the guardian watcher v6 does not run; run observe-v6 first"
  [[ -x "$(harness_binary)" ]] || build_harness
  if harness_is_running canary; then stop_harness canary; fi
  run_harness canary "$CORE_V7_PACKAGE_ID" "" 3600
  check_input_contract_created_under CoreState "$CORE_V6_PACKAGE_ID"
  check_input_contract_created_under Emitter "$CORE_V6_PACKAGE_ID"
  local v7_summary v6_summary v7_update_id v6_update_id
  v7_summary="$(publish_and_check v7-during-migration "$CORE_V7_PACKAGE_ID:Wormhole.Core.State:Emitter" "$CORE_V7_PACKAGE_ID")"
  v6_summary="$(publish_and_check v6-legacy-integrator "$CORE_V6_PACKAGE_ID:Wormhole.Core.State:Emitter" "$CORE_V6_PACKAGE_ID")"
  v7_update_id="$(jq -r .updateId <<<"$v7_summary")"
  v6_update_id="$(jq -r .updateId <<<"$v6_summary")"
  check_publish_package_on_each_host "$v7_update_id" "$CORE_V7_PACKAGE_ID"
  append_state_list v7DroppedUpdateIds "$v7_update_id"
  record_state canaryLegacyUpdateId "$v6_update_id"
  echo "  positive controls in the same window:"
  await_observation v6 "$(jq -r .sequence <<<"$v6_summary")" "$(jq -r .payload <<<"$v6_summary")" false
  await_observation canary "$(jq -r .sequence <<<"$v7_summary")" "$(jq -r .payload <<<"$v7_summary")" false
  sleep "$DROP_WINDOW_SECONDS"
  check_no_observation v6 "$v7_update_id"
  check_no_observation canary "$v6_update_id"
  check_drop_logged v6 "$v7_update_id" "$CORE_V7_PACKAGE_ID"
  check_drop_logged canary "$v6_update_id" "$CORE_V6_PACKAGE_ID"
}

# run_ntt_e2e_under_v7 <upload-dars: true|false>: ntt_e2e.sh with its watcher pinned to 0.7.0, then the package
# that ran each NTT PublishMessage, as each host sees it.
run_ntt_e2e_under_v7() {
  local deployment_key update_id
  NTT_UPLOAD_DARS="$1" NTT_CORE_PACKAGE_ID="$CORE_V7_PACKAGE_ID" NTT_GUARDIAN_SET_INDEX="$CURRENT_GUARDIAN_SET_INDEX" \
    NTT_GUARDIAN_KEY_INDICES="$CURRENT_GUARDIAN_KEY_INDICES" "$LOCALNET_SCRIPT_DIR/ntt_e2e.sh"
  for deployment_key in burnMint.burnUpdateId lockUnlock.lockUpdateId; do
    update_id="$(jq -er ".$deployment_key" "$NTT_CONTRACTS")"
    check_publish_package_on_each_host "$update_id" "$CORE_V7_PACKAGE_ID"
  done
  echo "  PASS: each NTT outbound PublishMessage ran under ${CORE_V7_PACKAGE_ID:0:12} on each host"
}

step_ntt() {
  echo "### 4. ntt_e2e.sh with ntt 0.3.0 and its watcher pinned to 0.7.0"
  harness_is_running v6 || fail "the guardian watcher v6 does not run; run observe-v6 first"
  run_ntt_e2e_under_v7 false
}

step_gate() {
  echo "### 5. Gate: the canary counts the one legacy 0.6.0 publish and no other"
  harness_is_running canary || fail "the canary watcher does not run; run publish-v7 first"
  stop_harness canary
  check_harness_reobservation_errors canary
  local expected_counts
  expected_counts="$(jq -cn --arg v6 "$CORE_V6_PACKAGE_ID" '{($v6): 1}')"
  [[ "$(jq -c . "$WATCHER_DIR/canary.metrics.json")" == "$expected_counts" ]] \
    || fail "canary unpinned-publish counts $(cat "$WATCHER_DIR/canary.metrics.json"), expected $expected_counts"
  [[ "$(harness_dropped_update_ids canary "$CORE_V6_PACKAGE_ID")" == "$(read_state canaryLegacyUpdateId)" ]] \
    || fail "the canary WARN logs name $(harness_dropped_update_ids canary "$CORE_V6_PACKAGE_ID" | tr '\n' ' '), expected $(read_state canaryLegacyUpdateId)"
  echo "  PASS: canary counter {0.6.0 ${CORE_V6_PACKAGE_ID:0:12}: 1}; WARN log names the legacy update $(read_state canaryLegacyUpdateId)"
  echo "  the NTT run of step 4 added no 0.6.0 publish"
}

# managers_created_by_ntt_v2: burn/mint NttManagers that ntt 0.2.0 created, whose admin key LocalNet holds, as JSON.
managers_created_by_ntt_v2() {
  active_contracts "$SUBMITTING_VALIDATOR" "$GG_PARTY" ":Wormhole.Ntt.Manager:NttManager" \
    | jq -c --arg ntt2 "$NTT_V2_PACKAGE_ID" '[.[] | select((.templateId | startswith($ntt2 + ":")) and (.createArgument.admin | startswith("NttBurnMintAdmin-")))]'
}

# set_paused_on_manager <command-id> <template-id> <manager-created-event-json>: SetPaused with the current value.
set_paused_on_manager() {
  local manager="$3" admin_party admin_label
  admin_party="$(jq -r .createArgument.admin <<<"$manager")"
  admin_label="$(sed -E 's/^NttBurnMintAdmin-([0-9]+)::.*/\1/' <<<"$admin_party")"
  submit_interactive "$1" "$(jq -n --arg admin "$admin_party" '[$admin]')" \
    "$(exercise_command "$2" "$(jq -r .contractId <<<"$manager")" SetPaused \
      "$(jq -c '{newPaused: .createArgument.paused}' <<<"$manager")")" \
    "$(external_party_signers_json "$admin_party" "$NTT_KEY_DIR/burn-mint-admin-$admin_label.key" "${admin_party#*::}")"
}

ntt_v2_manager_with_admin_key() {
  local manager_index managers admin_label
  managers="$(managers_created_by_ntt_v2)"
  for ((manager_index = 0; manager_index < $(jq length <<<"$managers"); manager_index++)); do
    admin_label="$(jq -r --argjson index "$manager_index" '.[$index].createArgument.admin' <<<"$managers" | sed -E 's/^NttBurnMintAdmin-([0-9]+)::.*/\1/')"
    if [[ -f "$NTT_KEY_DIR/burn-mint-admin-$admin_label.key" ]]; then
      jq -c --argjson index "$manager_index" '.[$index]' <<<"$managers"
      return 0
    fi
  done
  fail "no NttManager created by ntt 0.2.0 with a burn/mint admin key in $NTT_KEY_DIR"
}

step_unvet_v6() {
  echo "### 6. Unvet core 0.6.0 and ntt 0.2.0 together on each participant, immediately before the switch"
  harness_is_running v6 || fail "the guardian watcher v6 does not run; run observe-v6 first"
  harness_is_running canary && fail "the canary runs; run the gate step first"
  local validator_name emitter_before emitter_after manager v7_summary v7_update_id
  for validator_name in "${GUARDIAN_HOSTS[@]}"; do
    change_vetting "$validator_name" Unvet "$NTT_V2_PACKAGE_ID" "$CORE_V6_PACKAGE_ID"
  done
  for validator_name in "${GUARDIAN_HOSTS[@]}"; do
    await_vetting "$validator_name" wormhole-core "$(versions_json "0.7.0 $CORE_V7_PACKAGE_ID")"
    await_vetting "$validator_name" ntt "$(versions_json "0.3.0 $NTT_V3_PACKAGE_ID")"
  done
  record_state unvetOffset "$(ledger_end_of "$SUBMITTING_VALIDATOR")"

  echo "### 6a. A 0.6.0 PublishMessage fails at prepare and changes no contract"
  emitter_before="$(operator_core_contract Emitter | jq -c '{contractId, sequence: .createArgument.sequence}')"
  if publish_on_operator_emitter "core-upgrade-unvetted-v6-$(date +%s)" "$CORE_V6_PACKAGE_ID:Wormhole.Core.State:Emitter" \
      1 "$(hex_of "core upgrade after the unvet")" > "$gg_work_dir/unvetted-v6.out" 2> "$gg_work_dir/unvetted-v6.err"; then
    fail "a 0.6.0 PublishMessage committed after the unvet"
  fi
  expect_unvetted_rejection "$gg_work_dir/unvetted-v6" "$CORE_V6_PACKAGE_ID" "0.6.0 PublishMessage after the unvet"
  emitter_after="$(operator_core_contract Emitter | jq -c '{contractId, sequence: .createArgument.sequence}')"
  [[ "$emitter_before" == "$emitter_after" ]] || fail "the Operator Emitter changed: $emitter_before -> $emitter_after"
  echo "  PASS: the Operator Emitter is unchanged: $(jq -c '{contractId: .contractId[0:16], sequence}' <<<"$emitter_after")"

  echo "### 6b. An ntt 0.2.0 exercise on an NttManager fails at prepare"
  manager="$(ntt_v2_manager_with_admin_key)"
  record_state nttV2ManagerContractId "$(jq -r .contractId <<<"$manager")"
  if set_paused_on_manager "core-upgrade-unvetted-ntt-v2-$(date +%s)" "$NTT_V2_PACKAGE_ID:Wormhole.Ntt.Manager:NttManager" "$manager" \
      > "$gg_work_dir/unvetted-ntt-v2.out" 2> "$gg_work_dir/unvetted-ntt-v2.err"; then
    fail "an ntt 0.2.0 SetPaused committed after the unvet"
  fi
  expect_unvetted_rejection "$gg_work_dir/unvetted-ntt-v2" "$NTT_V2_PACKAGE_ID" "ntt 0.2.0 SetPaused after the unvet"

  echo "### 6c. A 0.7.0 publish on the Emitter that 0.6.0 created, before the switch: v6 drops it"
  check_input_contract_created_under Emitter "$CORE_V6_PACKAGE_ID"
  check_input_contract_created_under CoreState "$CORE_V6_PACKAGE_ID"
  v7_summary="$(publish_and_check v7-before-switch "$CORE_V7_PACKAGE_ID:Wormhole.Core.State:Emitter" "$CORE_V7_PACKAGE_ID")"
  v7_update_id="$(jq -r .updateId <<<"$v7_summary")"
  append_state_list v7DroppedUpdateIds "$v7_update_id"
  sleep "$DROP_WINDOW_SECONDS"
  check_no_observation v6 "$v7_update_id"
  check_drop_logged v6 "$v7_update_id" "$CORE_V7_PACKAGE_ID"
}

step_switch_v7() {
  echo "### 7. Coordinated restart: stop v6, enumerate the dropped updates, start v7 with reobservation requests"
  harness_is_running v6 || fail "the guardian watcher v6 does not run; run observe-v6 first"
  stop_harness v6
  check_harness_reobservation_errors v6
  local switch_offset warn_update_ids scan_update_ids live_summary update_id expected_v6_drops
  local v6_scan_update_ids v6_observed_update_ids
  switch_offset="$(ledger_end_of "$SUBMITTING_VALIDATOR")"
  record_state switchOffset "$switch_offset"
  warn_update_ids="$(harness_dropped_update_ids v6 "$CORE_V7_PACKAGE_ID")"
  scan_update_ids="$(scan_publish_update_ids "$(read_state upgradeStartOffset)" "$switch_offset" "$CORE_V7_PACKAGE_ID")"
  echo "  v6 WARN logs: $(wc -l <<<"$warn_update_ids" | tr -d ' ') dropped 0.7.0 updates"
  echo "  scan as $WATCHER_USER_ID, offsets ($(read_state upgradeStartOffset), $switch_offset]: $(wc -l <<<"$scan_update_ids" | tr -d ' ') 0.7.0 PublishMessage updates"
  [[ -n "$scan_update_ids" ]] || fail "the scan found no 0.7.0 PublishMessage update"
  [[ "$warn_update_ids" == "$scan_update_ids" ]] || fail "the WARN logs and the scan differ: $(diff <(echo "$warn_update_ids") <(echo "$scan_update_ids") | tr '\n' ' ')"
  for update_id in $(jq -r '.v7DroppedUpdateIds[]' "$UPGRADE_STATE"); do
    grep -qxF "$update_id" <<<"$scan_update_ids" || fail "the scan does not list $update_id"
  done
  expected_v6_drops="$(wc -l <<<"$scan_update_ids" | tr -d ' ')"
  [[ "$(jq -c . "$WATCHER_DIR/v6.metrics.json")" == "$(jq -cn --arg v7 "$CORE_V7_PACKAGE_ID" --argjson count "$expected_v6_drops" '{($v7): $count}')" ]] \
    || fail "v6 unpinned-publish counts $(cat "$WATCHER_DIR/v6.metrics.json"), expected $expected_v6_drops for 0.7.0"
  echo "  PASS: WARN logs, scan and v6 counter agree: $expected_v6_drops dropped 0.7.0 updates, the recorded ones included"
  printf '%s\n' "$scan_update_ids" > "$WATCHER_DIR/dropped-update-ids.txt"
  v6_scan_update_ids="$(scan_publish_update_ids "$(read_state upgradeStartOffset)" "$switch_offset" "$CORE_V6_PACKAGE_ID")"
  v6_observed_update_ids="$(jq -r 'select(.isReobservation | not) | .updateId' "$WATCHER_DIR/v6.jsonl" \
    | grep -xF -f <(printf '%s\n' "$v6_scan_update_ids") | sort -u || true)"
  [[ -n "$v6_scan_update_ids" ]] || fail "the scan found no 0.6.0 PublishMessage update; publish-v7 makes one"
  [[ "$v6_observed_update_ids" == "$v6_scan_update_ids" ]] \
    || fail "0.6.0 updates in the scan that v6 did not observe: $(comm -23 <(echo "$v6_scan_update_ids") <(echo "$v6_observed_update_ids") | tr '\n' ' ')"
  echo "  PASS: v6 observed each of the $(wc -l <<<"$v6_scan_update_ids" | tr -d ' ') 0.6.0 PublishMessage updates of the scan before it stopped"

  if harness_is_running v7; then stop_harness v7; fi
  run_harness v7 "$CORE_V7_PACKAGE_ID" "$(paste -sd, - <<<"$scan_update_ids")" 900
  live_summary="$(publish_and_check v7-after-switch "$EMITTER_TEMPLATE" "$CORE_V7_PACKAGE_ID")"
  await_observation v7 "$(jq -r .sequence <<<"$live_summary")" "$(jq -r .payload <<<"$live_summary")" false
  stop_harness v7
  check_harness_reobservation_errors v7
  [[ "$(jq -sr '[.[] | select(.isReobservation) | .updateId] | unique | .[]' "$WATCHER_DIR/v7.jsonl")" == "$scan_update_ids" ]] \
    || fail "v7 reobserved $(jq -sc '[.[] | select(.isReobservation) | .updateId] | unique' "$WATCHER_DIR/v7.jsonl"), expected each scanned update"
  [[ "$(jq -c . "$WATCHER_DIR/v7.metrics.json")" == "{}" ]] || fail "v7 dropped publishes: $(cat "$WATCHER_DIR/v7.metrics.json")"
  echo "  PASS: v7 reobserved each of the $expected_v6_drops dropped updates, observed the live publish, and dropped none"
}

step_enforce_v7() {
  echo "### 8. Nonce $NONCE_ABOVE_UINT32 (above uint32): 0.7.0 rejects it; 0.6.0 commits it only while vetted"
  local payload_hex summary validator_name v6_vetted_hosts=0
  payload_hex="$(hex_of "core upgrade nonce check")"
  if publish_on_operator_emitter "core-upgrade-nonce-v7-$(date +%s)" "$CORE_V7_PACKAGE_ID:Wormhole.Core.State:Emitter" \
      "$NONCE_ABOVE_UINT32" "$payload_hex" > "$gg_work_dir/nonce-v7.out" 2> "$gg_work_dir/nonce-v7.err"; then
    fail "0.7.0 committed a nonce above uint32"
  fi
  expect_daml_failure "$gg_work_dir/nonce-v7" "nonce exceeds uint32" "0.7.0 PublishMessage with nonce $NONCE_ABOVE_UINT32"
  for validator_name in "${GUARDIAN_HOSTS[@]}"; do
    if package_is_vetted "$validator_name" wormhole-core "$CORE_V6_PACKAGE_ID"; then v6_vetted_hosts=$((v6_vetted_hosts + 1)); fi
  done
  if [[ "$v6_vetted_hosts" -eq "${#GUARDIAN_HOSTS[@]}" ]]; then
    summary="$(publish_summary "$(publish_on_operator_emitter "core-upgrade-nonce-v6-$(date +%s)" \
      "$CORE_V6_PACKAGE_ID:Wormhole.Core.State:Emitter" "$NONCE_ABOVE_UINT32" "$payload_hex")")"
    [[ "$(jq -r .executingPackageId <<<"$summary")" == "$CORE_V6_PACKAGE_ID" ]] || fail "the nonce publish did not execute under 0.6.0"
    echo "  0.6.0 vetted on each host: committed under 0.6.0: $summary"
  elif [[ "$v6_vetted_hosts" -eq 0 ]]; then
    if publish_on_operator_emitter "core-upgrade-nonce-v6-$(date +%s)" "$CORE_V6_PACKAGE_ID:Wormhole.Core.State:Emitter" \
        "$NONCE_ABOVE_UINT32" "$payload_hex" > "$gg_work_dir/nonce-v6.out" 2> "$gg_work_dir/nonce-v6.err"; then
      fail "0.6.0 committed a nonce above uint32 after the unvet"
    fi
    expect_unvetted_rejection "$gg_work_dir/nonce-v6" "$CORE_V6_PACKAGE_ID" "0.6.0 PublishMessage with nonce $NONCE_ABOVE_UINT32 after the unvet"
  else
    fail "$v6_vetted_hosts of ${#GUARDIAN_HOSTS[@]} hosts vet 0.6.0; expected all or none"
  fi
}

step_post_unvet() {
  echo "### 9a. Each host vets only core 0.7.0 and ntt 0.3.0"
  local validator_name transaction summary manager verified_vaa verify_event
  for validator_name in "${GUARDIAN_HOSTS[@]}"; do
    await_vetting "$validator_name" wormhole-core "$(versions_json "0.7.0 $CORE_V7_PACKAGE_ID")"
    await_vetting "$validator_name" ntt "$(versions_json "0.3.0 $NTT_V3_PACKAGE_ID")"
  done

  echo "### 9b. PublishMessage by package name on the 0.6.0 CoreState executes under 0.7.0"
  check_input_contract_created_under CoreState "$CORE_V6_PACKAGE_ID"
  summary="$(publish_and_check after-unvet "$EMITTER_TEMPLATE" "$CORE_V7_PACKAGE_ID")"
  echo "  PASS: $(jq -c '{executingPackageId: .executingPackageId[0:12], sequence}' <<<"$summary")"

  echo "### 9c. ParseAndVerifyVAA on the 0.6.0 CoreState executes under 0.7.0"
  check_input_contract_created_under CoreState "$CORE_V6_PACKAGE_ID"
  verified_vaa="$(generate_vaa -kind message -signing-set-index "$CURRENT_GUARDIAN_SET_INDEX" \
    -signing-key-indices "$CURRENT_GUARDIAN_KEY_INDICES" -sequence "$(date +%s)")"
  transaction="$(verify_vaa_as_operator "core-upgrade-verify-$(date +%s)" "$(jq -r .vaaHex <<<"$verified_vaa")" "$(vaa_pub_keys_json "$verified_vaa")")"
  verify_event="$(jq -ec '[.events[] | .ExercisedEvent? // empty | select(.choice == "ParseAndVerifyVAA")]
    | if length == 1 then .[0] | {executingPackageId: (.templateId | split(":")[0]), guardianSetIndex: .exerciseResult.guardianSetIndex}
      else error("expected one ParseAndVerifyVAA event") end' <<<"$transaction")"
  [[ "$(jq -r .executingPackageId <<<"$verify_event")" == "$CORE_V7_PACKAGE_ID" ]] || fail "ParseAndVerifyVAA did not execute under 0.7.0: $verify_event"
  echo "  PASS: $(jq -c '.executingPackageId |= .[0:12]' <<<"$verify_event")"

  echo "### 9d. SetPaused by package name on an NttManager that ntt 0.2.0 created executes under ntt 0.3.0"
  manager="$(ntt_v2_manager_with_admin_key)"
  echo "  input NttManager $(jq -r '.contractId[0:16]' <<<"$manager") created under ntt 0.2.0 $(jq -r '.templateId[0:12]' <<<"$manager")"
  transaction="$(set_paused_on_manager "core-upgrade-ntt-v2-manager-$(date +%s)" "$NTT_MANAGER_TEMPLATE" "$manager")"
  [[ "$(jq -r '[.events[] | .ExercisedEvent? // empty | select(.choice == "SetPaused") | .templateId | split(":")[0]] | unique | .[]' <<<"$transaction")" == "$NTT_V3_PACKAGE_ID" ]] \
    || fail "SetPaused did not execute under ntt 0.3.0"
  [[ "$(jq -r '[.events[] | .CreatedEvent? // empty | select(.templateId | endswith(":Wormhole.Ntt.Manager:NttManager")) | .templateId | split(":")[0]] | .[]' <<<"$transaction")" == "$NTT_V3_PACKAGE_ID" ]] \
    || fail "the successor NttManager was not created under ntt 0.3.0"
  echo "  PASS: SetPaused executed under ntt 0.3.0 ${NTT_V3_PACKAGE_ID:0:12}; the successor NttManager is an ntt 0.3.0 contract"

  echo "### 9e. ntt_e2e.sh with ntt 0.3.0 and a watcher pinned to 0.7.0"
  run_ntt_e2e_under_v7 true
}

copy_harness_evidence() {
  local evidence_copy_dir="${EVIDENCE_LOG%.log}"
  mkdir -p "$evidence_copy_dir"
  cp "$WATCHER_DIR"/*.jsonl "$WATCHER_DIR"/*.log "$WATCHER_DIR"/*.metrics.json "$WATCHER_DIR"/*.txt "$evidence_copy_dir"/ 2>/dev/null || true
  cp "$UPGRADE_STATE" "$evidence_copy_dir"/
}

on_exit() {
  local exit_status=$?
  if [[ "$exit_status" -ne 0 ]]; then stop_started_harnesses; fi
  copy_harness_evidence
  rm -rf "$gg_work_dir"
}
trap on_exit EXIT

case "${1:-}" in
  prepare) step_prepare ;;
  observe-v6) step_observe_v6 ;;
  vet-v7) step_vet_v7 ;;
  publish-v7) step_publish_v7 ;;
  ntt) step_ntt ;;
  gate) step_gate ;;
  unvet-v6) step_unvet_v6 ;;
  switch-v7) step_switch_v7 ;;
  enforce-v7) step_enforce_v7 ;;
  post-unvet) step_post_unvet ;;
  all)
    step_prepare
    step_observe_v6
    step_vet_v7
    step_publish_v7
    step_ntt
    step_gate
    step_unvet_v6
    step_switch_v7
    step_enforce_v7
    step_post_unvet
    ;;
  *)
    echo "usage: core_upgrade_rehearsal.sh prepare|observe-v6|vet-v7|publish-v7|ntt|gate|unvet-v6|switch-v7|enforce-v7|post-unvet|all" >&2
    exit 2
    ;;
esac
echo "core_upgrade_rehearsal.sh ${1}: PASS"
