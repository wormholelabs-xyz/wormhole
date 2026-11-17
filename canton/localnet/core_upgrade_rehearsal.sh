#!/usr/bin/env bash
# Rehearse the wormhole-core 0.6.0 to 0.7.0 upgrade (canton/README.md section 7.5) on LocalNet, in the
# procedure order: dependents first, unvet the old versions immediately before the watcher switch.
# Each guardian watcher runs as three harnesses, one on each participant, each read as guardianGovernance there.
# Steps (run one, or "all" in order):
#   prepare       Start state: core 0.6.0 and ntt 0.2.0 vetted, core 0.7.0 and ntt 0.3.0 not vetted, on each participant.
#   observe-v6    Start the guardian watchers v6-<host>, pinned to 0.6.0. Publish under 0.6.0. Expect three observations.
#   vet-v7        Record the start offset on each participant. Upload and vet core 0.7.0, then ntt 0.3.0, on each one.
#   publish-v7    Start the canary watcher, pinned to 0.7.0. Publish under 0.7.0 and under 0.6.0 in one window.
#                 Expect each watcher to observe one message, and to drop and count the other.
#   ntt           ntt_e2e.sh with ntt 0.3.0. Expect each NTT PublishMessage to run under 0.7.0 on each host.
#   gate          Quiet window: the canary counter sum over all labels does not change, the canary observes a control
#                 publish after the window, and a scan on each participant finds no publish under another package.
#   unvet-v6      With v6-<host> still pinned to 0.6.0: unvet core 0.6.0 and ntt 0.2.0 together on each participant.
#                 Expect 0.6.0 and ntt 0.2.0 submissions to fail at prepare. Then the deterministic drain gate: wait the
#                 synchronizer timeouts, publish a 0.7.0 control that each v6-<host> drops, and scan each participant
#                 for a 0.6.0 publish that its v6 watcher did not observe.
#   switch-v7     Stop v6-<host> (the coordinated restart). On each participant, the WARN logs and a scan since its
#                 start offset give equal dropped update id sets. Start v7-<host>, pinned to 0.7.0, with those
#                 reobservation requests. Expect each dropped update and one live 0.7.0 publish on each host.
#   enforce-v7    A nonce above uint32: 0.7.0 rejects it. 0.6.0 commits it while vetted, and fails after the unvet.
#   post-unvet    Contracts that 0.6.0 and ntt 0.2.0 created work under 0.7.0 and ntt 0.3.0; ntt_e2e.sh passes.
#   rollback      With 0.6.0 unvetted, a watcher pinned to 0.6.0 reobserves an old 0.6.0 update. Then re-vet core 0.6.0
#                 and ntt 0.2.0 on each participant. Expect 0.6.0 and ntt 0.2.0 exercises on contracts that 0.7.0 and
#                 ntt 0.3.0 created to commit, and the watchers rollback-v6-<host> to observe the 0.6.0 publish.
#   restore       Unvet core 0.6.0 and ntt 0.2.0 again on each participant. Expect a 0.6.0 publish to fail at prepare.
# Usage: core_upgrade_rehearsal.sh <step>|all
# A step that fails stops each watcher harness that it started. A step that passes leaves v6-<host> (observe-v6) and
# the canary (publish-v7) running for the next steps.
# Precondition: the LocalNet CoreState was created under core 0.6.0 (a LocalNet from before the 0.8.0 genesis);
# gg_publish_and_govern.sh ran; the CoreState message fee is 0; NTT_V3_DAR is an ntt 0.3.0 build on core 0.7.0;
# LocalNet has the ntt 0.2.0 package 8b9c3ee1 and a burn/mint NttManager that it created.
# A LocalNet with the 0.8.0 genesis cannot run this rehearsal: its contracts cannot run under 0.6.0 or 0.7.0.
# The prepare step stops on such a LocalNet before it changes the vetting state.
set -euo pipefail
# shellcheck source=ntt_lib.sh
source "$(dirname "${BASH_SOURCE[0]}")/ntt_lib.sh"
# shellcheck source=core_upgrade_package_ids.sh
source "$LOCALNET_SCRIPT_DIR/core_upgrade_package_ids.sh"

readonly UPGRADE_STATE="$LOCALNET_STATE_DIR/core-upgrade.json"
readonly WATCHER_DIR="$LOCALNET_STATE_DIR/core-upgrade-watcher"
readonly OBSERVATION_WAIT_SECONDS=60
# Time that a watcher gets to show that it drops a message.
readonly DROP_WINDOW_SECONDS=15
readonly NONCE_ABOVE_UINT32=4294967296
# Length of the quiet window of the canary gate. Production uses the agreed time.
readonly QUIET_WINDOW_SECONDS="${QUIET_WINDOW_SECONDS:-20}"
# Added to the synchronizer timeouts in the drain gate after the unvet.
readonly DRAIN_MARGIN_SECONDS=30
readonly UPDATE_LOOKUP_ATTEMPTS=30
readonly UNVETTED_ERROR_CODE="INVALID_PRESCRIBED_SYNCHRONIZER_ID"
# Current guardian set of the LocalNet CoreState (after gg_guardian_set_rotation.sh 1 2).
readonly CURRENT_GUARDIAN_SET_INDEX="${CURRENT_GUARDIAN_SET_INDEX:-2}"
readonly CURRENT_GUARDIAN_KEY_INDICES="${CURRENT_GUARDIAN_KEY_INDICES:-2,3,4}"
# shellcheck source=watcher_harness_lib.sh
source "$LOCALNET_SCRIPT_DIR/watcher_harness_lib.sh"
mkdir -p "$WATCHER_DIR"
[[ -f "$UPGRADE_STATE" ]] || echo '{}' > "$UPGRADE_STATE"

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

# record_host_offset <key> <validator> <offset>: offsets are per participant.
record_host_offset() {
  local updated
  updated="$(jq --arg key "$1" --arg host "$2" --arg offset "$3" '.[$key][$host] = $offset' "$UPGRADE_STATE")"
  printf '%s\n' "$updated" > "$UPGRADE_STATE"
}

read_host_offset() {
  jq -er --arg key "$1" --arg host "$2" '.[$key][$host]' "$UPGRADE_STATE"
}

# record_ledger_end_of_each_host <key>
record_ledger_end_of_each_host() {
  local validator_name offset
  for validator_name in "${GUARDIAN_HOSTS[@]}"; do
    offset="$(ledger_end_of "$validator_name")"
    record_host_offset "$1" "$validator_name" "$offset"
    echo "  $1 on $validator_name: $offset"
  done
}

check_dar_package_ids() {
  check_rehearsal_dar "$CORE_V6_DAR" "wormhole-core 0.6.0" "$CORE_V6_PACKAGE_ID" || fail "core 0.6.0 DAR check failed"
  check_rehearsal_dar "$CORE_V7_DAR" "wormhole-core 0.7.0" "$CORE_V7_PACKAGE_ID" || fail "core 0.7.0 DAR check failed"
}

# check_ledger_predates_core_v8: the Operator CoreState is a 0.6.0 contract. A 0.8.0 genesis CoreState fails here.
check_ledger_predates_core_v8() {
  local created_package
  created_package="$(operator_core_contract CoreState | jq -r '.templateId | split(":")[0]')"
  [[ "$created_package" == "$CORE_V6_PACKAGE_ID" ]] \
    || fail "the Operator CoreState was created under ${created_package:0:12}, expected core 0.6.0 ${CORE_V6_PACKAGE_ID:0:12}. This rehearsal needs a LocalNet from before the 0.8.0 genesis (localnet/README.md)"
  echo "  the Operator CoreState is a core 0.6.0 contract"
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

# vetted_package_ids <validator> <package-name>: package ids of each vetted version, one per line.
vetted_package_ids() {
  vetted_versions "$1" "$2" | jq -r '.[] | split(" ")[1]'
}

package_is_uploaded() {
  curl -sSf "$(json_api_url "$1")/v2/packages" -H "@$ADMIN_AUTH_HEADER" | jq -e --arg id "$2" '.packageIds | index($id) != null' > /dev/null
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

# update_by_id_on <validator> <update-id>: the transaction as guardianGovernance sees it on that participant. Retries
# while the participant has not yet published the update.
update_by_id_on() {
  local attempt
  for ((attempt = 1; attempt <= UPDATE_LOOKUP_ATTEMPTS; attempt++)); do
    if fetch_update_by_id_on "$1" "$2"; then return 0; fi
    sleep 1
  done
  fail "$1 has no update $2 after $UPDATE_LOOKUP_ATTEMPTS attempts"
}

fetch_update_by_id_on() {
  curl -sSf -X POST "$(json_api_url "$1")/v2/updates/update-by-id" -H "@$SCRIPT_AUTH_HEADER" \
    -H 'Content-Type: application/json' \
    -d "$(jq -n --arg id "$2" --arg gg "$GG_PARTY" '{updateId: $id, updateFormat: {includeTransactions: {
      eventFormat: {filtersByParty: {($gg): {cumulative: [{identifierFilter: {WildcardFilter: {value: {includeCreatedEventBlob: false}}}}]}}, verbose: false},
      transactionShape: "TRANSACTION_SHAPE_LEDGER_EFFECTS"}}}')" 2>/dev/null \
    | jq -ec '.update.Transaction.value'
}

# update_offset_on <validator> <update-id>: the participant offset of the update.
update_offset_on() {
  update_by_id_on "$1" "$2" | jq -er .offset
}

# publish_package_on <validator> <update-id>: package id of the PublishMessage event and the participant offset, as guardianGovernance sees them there.
publish_package_on() {
  update_by_id_on "$1" "$2" \
    | jq -er '([.events[] | .ExercisedEvent? // empty | select(.choice == "PublishMessage") | .templateId | split(":")[0]] | unique) as $packages
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

# scan_publishes <validator> <begin-exclusive-offset> <end-inclusive-offset>: publish_message_scan.sh as the watcher
# user on that participant. One JSON line per PublishMessage of any package.
scan_publishes() {
  local scan_token_file="$gg_work_dir/watcher-scan.jwt"
  ensure_watcher_user "$1" >&2
  (umask 077 && "$LOCALNET_SCRIPT_DIR/token.sh" "$WATCHER_USER_ID" > "$scan_token_file")
  "$LOCALNET_SCRIPT_DIR/publish_message_scan.sh" "$(json_api_url "$1")" "$scan_token_file" "$GG_PARTY" "$2" "$3"
}

# scan_publish_update_ids <validator> <begin-exclusive> <end-inclusive> <executing-package-id>: sorted update ids
# with a PublishMessage event of that package.
scan_publish_update_ids() {
  scan_publishes "$1" "$2" "$3" | jq -r --arg package "$4" 'select(.executingPackageId == $package) | .updateId' | sort -u
}

# scan_publish_update_ids_not_under <validator> <begin-exclusive> <end-inclusive> <package-id>: sorted update ids with
# a PublishMessage event of any other package. This is the scan equivalent of the counter sum over all labels.
scan_publish_update_ids_not_under() {
  scan_publishes "$1" "$2" "$3" | jq -r --arg package "$4" 'select(.executingPackageId != $package) | .updateId' | sort -u
}

# observed_update_ids <harness-name>: sorted update ids of the live observations of the harness.
observed_update_ids() {
  if [[ -s "$WATCHER_DIR/$1.jsonl" ]]; then
    jq -r 'select(.isReobservation | not) | .updateId' "$WATCHER_DIR/$1.jsonl" | sort -u
  fi
}

# unpinned_publish_total <harness-name>: sum of the unpinned-publish counter over all executing_package_id labels,
# "other" included. The harness rewrites the metrics file every 500 ms.
unpinned_publish_total() {
  jq -e 'add // 0' "$WATCHER_DIR/$1.metrics.json"
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

# await_observation_of_update <harness-name> <update-id> <reobservation true|false>
await_observation_of_update() {
  local observations_file="$WATCHER_DIR/$1.jsonl" waited_seconds
  for ((waited_seconds = 0; waited_seconds < OBSERVATION_WAIT_SECONDS; waited_seconds++)); do
    if [[ -s "$observations_file" ]] && jq -se --arg id "$2" --argjson reobserved "$3" \
        'any(.[]; .updateId == $id and .isReobservation == $reobserved)' "$observations_file" > /dev/null; then
      echo "  watcher $1 observed update $2 (reobservation $3)"
      return 0
    fi
    sleep 1
  done
  fail "watcher $1 has no observation of update $2 (reobservation $3) after ${OBSERVATION_WAIT_SECONDS}s"
}

# check_no_observation <harness-name> <update-id>
check_no_observation() {
  if [[ -s "$WATCHER_DIR/$1.jsonl" ]] && jq -se --arg id "$2" 'any(.[]; .updateId == $id)' "$WATCHER_DIR/$1.jsonl" > /dev/null; then
    fail "watcher $1 observed update $2"
  fi
  echo "  PASS: watcher $1 has no observation of update $2"
}

# check_drop_logged <harness-name> <update-id> <executing-package-id>: waits up to OBSERVATION_WAIT_SECONDS.
check_drop_logged() {
  local waited_seconds
  for ((waited_seconds = 0; waited_seconds < OBSERVATION_WAIT_SECONDS; waited_seconds++)); do
    if harness_dropped_update_ids "$1" "$3" | grep -qxF "$2"; then
      echo "  PASS: watcher $1 WARN log: dropped $2, executing package ${3:0:12}"
      return 0
    fi
    sleep 1
  done
  fail "watcher $1 has no WARN log for the drop of $2 (${3:0:12})"
}

# v6_watchers_run: each v6-<host> runs.
v6_watchers_run() {
  local validator_name
  for validator_name in "${GUARDIAN_HOSTS[@]}"; do
    harness_is_running "v6-$validator_name" || return 1
  done
}

# start_watcher_on_each_host <name-prefix> <core-package-id> <reobserve-update-ids> <max-seconds>
start_watcher_on_each_host() {
  local validator_name
  for validator_name in "${GUARDIAN_HOSTS[@]}"; do
    if harness_is_running "$1-$validator_name"; then stop_harness "$1-$validator_name"; fi
    run_harness_on "$validator_name" "$1-$validator_name" "$2" "$3" "$4"
  done
}

# stop_watcher_on_each_host <name-prefix>
stop_watcher_on_each_host() {
  local validator_name
  for validator_name in "${GUARDIAN_HOSTS[@]}"; do
    stop_harness "$1-$validator_name"
    check_harness_reobservation_errors "$1-$validator_name"
  done
}

# check_watchers_agree <name-prefix>: the three harnesses have equal sets of (update id, reobservation) observations.
check_watchers_agree() {
  local validator_name reference_set="" observation_set
  for validator_name in "${GUARDIAN_HOSTS[@]}"; do
    observation_set="$(jq -r '"\(.updateId) \(.isReobservation)"' "$WATCHER_DIR/$1-$validator_name.jsonl" | sort -u)"
    if [[ -z "$reference_set" ]]; then
      reference_set="$observation_set"
    elif [[ "$observation_set" != "$reference_set" ]]; then
      fail "watchers $1-* differ: $1-$validator_name has $(wc -l <<<"$observation_set" | tr -d ' ') observations: $(diff <(echo "$reference_set") <(echo "$observation_set") | tr '\n' ' ')"
    fi
  done
  echo "  PASS: $1-{$(IFS=,; echo "${GUARDIAN_HOSTS[*]}")} agree on $(wc -l <<<"$reference_set" | tr -d ' ') observations (update id, reobservation)"
}

# drain_seconds_from_synchronizer_parameters: confirmation response timeout + mediator reaction timeout + ledger time
# record time tolerance + DRAIN_MARGIN_SECONDS, read from the dynamic synchronizer parameters of each participant.
drain_seconds_from_synchronizer_parameters() {
  local console_output parameter_name parameter_values total_seconds="$DRAIN_MARGIN_SECONDS" value_seconds
  console_output="$("$LOCALNET_SCRIPT_DIR/canton_console.sh" "$LOCALNET_SCRIPT_DIR/console/synchronizer_parameters.canton" 2>&1)" \
    || { echo "$console_output" >&2; fail "synchronizer_parameters.canton failed"; }
  for parameter_name in "confirmation response timeout" "mediator reaction timeout" "ledger time record time tolerance"; do
    parameter_values="$(grep -oE "^ *$parameter_name = [0-9]+[smh]," <<<"$console_output" | sed -E 's/.* = ([0-9]+[smh]),/\1/' | sort -u)"
    [[ "$(grep -c . <<<"$parameter_values")" -eq 1 ]] || fail "expected one value of '$parameter_name' on the participants, found: $parameter_values"
    case "$parameter_values" in
      *s) value_seconds="${parameter_values%s}" ;;
      *m) value_seconds="$((${parameter_values%m} * 60))" ;;
      *h) value_seconds="$((${parameter_values%h} * 3600))" ;;
    esac
    echo "  synchronizer parameter: $parameter_name = $parameter_values" >&2
    total_seconds=$((total_seconds + value_seconds))
  done
  echo "$total_seconds"
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
  check_ledger_predates_core_v8
  local validator_name newer_packages
  local package_id
  for validator_name in "${GUARDIAN_HOSTS[@]}"; do
    newer_packages=()
    for package_id in $(vetted_package_ids "$validator_name" ntt); do
      [[ "$package_id" == "$NTT_V2_PACKAGE_ID" ]] || newer_packages+=("$package_id")
    done
    for package_id in $(vetted_package_ids "$validator_name" wormhole-core); do
      [[ "$package_id" == "$CORE_V6_PACKAGE_ID" ]] || newer_packages+=("$package_id")
    done
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
  echo "### 1. Guardian watchers v6-<host>, pinned to 0.6.0, one on each participant, observe a 0.6.0 PublishMessage"
  build_harness
  start_watcher_on_each_host v6 "$CORE_V6_PACKAGE_ID" "" 3600
  check_input_contract_created_under CoreState "$CORE_V6_PACKAGE_ID"
  local summary validator_name
  summary="$(publish_and_check v6-baseline "$CORE_V6_PACKAGE_ID:Wormhole.Core.State:Emitter" "$CORE_V6_PACKAGE_ID")"
  for validator_name in "${GUARDIAN_HOSTS[@]}"; do
    await_observation "v6-$validator_name" "$(jq -r .sequence <<<"$summary")" "$(jq -r .payload <<<"$summary")" false
  done
}

step_vet_v7() {
  echo "### 2. Step 0: record the start offset on each participant. Vet core 0.7.0, then ntt 0.3.0, on each participant"
  check_dar_package_ids
  local validator_name
  record_ledger_end_of_each_host startOffset
  for validator_name in "${GUARDIAN_HOSTS[@]}"; do
    upload_and_vet "$validator_name" "$CORE_V7_DAR"
  done
  for validator_name in "${GUARDIAN_HOSTS[@]}"; do
    await_vetting "$validator_name" wormhole-core "$(versions_json "0.6.0 $CORE_V6_PACKAGE_ID" "0.7.0 $CORE_V7_PACKAGE_ID")"
  done
  vet_ntt_v3_on_each_host
  for validator_name in "${GUARDIAN_HOSTS[@]}"; do
    await_vetting "$validator_name" ntt "$(versions_json "0.2.0 $NTT_V2_PACKAGE_ID" "0.3.0 $NTT_V3_PACKAGE_ID")"
  done
}

# vet_ntt_v3_on_each_host: vet NTT_V3_PACKAGE_ID by id on each participant that has it. Upload and vet NTT_V3_DAR
# on each other participant.
vet_ntt_v3_on_each_host() {
  local validator_name
  for validator_name in "${GUARDIAN_HOSTS[@]}"; do
    if package_is_uploaded "$validator_name" "$NTT_V3_PACKAGE_ID"; then
      change_vetting "$validator_name" Vet "$NTT_V3_PACKAGE_ID"
    else
      [[ -n "$NTT_V3_DAR" ]] || fail "$validator_name does not have ntt 0.3.0 ${NTT_V3_PACKAGE_ID:0:12}; set NTT_V3_DAR"
      check_rehearsal_dar "$NTT_V3_DAR" "ntt 0.3.0" "$NTT_V3_PACKAGE_ID" || fail "ntt 0.3.0 DAR check failed"
      upload_and_vet "$validator_name" "$NTT_V3_DAR"
    fi
  done
}

step_publish_v7() {
  echo "### 3. Canary watcher pinned to 0.7.0. One 0.7.0 and one 0.6.0 publish on the 0.6.0 Emitter and CoreState"
  v6_watchers_run || fail "a guardian watcher v6-<host> does not run; run observe-v6 first"
  [[ -x "$(harness_binary)" ]] || build_harness
  if harness_is_running canary; then stop_harness canary; fi
  run_harness canary "$CORE_V7_PACKAGE_ID" "" 3600
  check_input_contract_created_under CoreState "$CORE_V6_PACKAGE_ID"
  check_input_contract_created_under Emitter "$CORE_V6_PACKAGE_ID"
  local v7_summary v6_summary v7_update_id v6_update_id validator_name
  v7_summary="$(publish_and_check v7-during-migration "$CORE_V7_PACKAGE_ID:Wormhole.Core.State:Emitter" "$CORE_V7_PACKAGE_ID")"
  v6_summary="$(publish_and_check v6-legacy-integrator "$CORE_V6_PACKAGE_ID:Wormhole.Core.State:Emitter" "$CORE_V6_PACKAGE_ID")"
  v7_update_id="$(jq -r .updateId <<<"$v7_summary")"
  v6_update_id="$(jq -r .updateId <<<"$v6_summary")"
  check_publish_package_on_each_host "$v7_update_id" "$CORE_V7_PACKAGE_ID"
  append_state_list v7DroppedUpdateIds "$v7_update_id"
  record_state canaryLegacyUpdateId "$v6_update_id"
  echo "  positive controls in the same window:"
  for validator_name in "${GUARDIAN_HOSTS[@]}"; do
    await_observation "v6-$validator_name" "$(jq -r .sequence <<<"$v6_summary")" "$(jq -r .payload <<<"$v6_summary")" false
  done
  await_observation canary "$(jq -r .sequence <<<"$v7_summary")" "$(jq -r .payload <<<"$v7_summary")" false
  sleep "$DROP_WINDOW_SECONDS"
  for validator_name in "${GUARDIAN_HOSTS[@]}"; do
    check_no_observation "v6-$validator_name" "$v7_update_id"
    check_drop_logged "v6-$validator_name" "$v7_update_id" "$CORE_V7_PACKAGE_ID"
  done
  check_no_observation canary "$v6_update_id"
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
  v6_watchers_run || fail "a guardian watcher v6-<host> does not run; run observe-v6 first"
  run_ntt_e2e_under_v7 false
}

step_gate() {
  echo "### 5. Canary gate: a quiet window with a positive control, backed by a scan on each participant"
  harness_is_running canary || fail "the canary watcher does not run; run publish-v7 first"
  v6_watchers_run || fail "a guardian watcher v6-<host> does not run; run observe-v6 first"
  local legacy_update_id counter_total_before counter_total_after control_summary control_update_id validator_name
  local control_offset old_package_update_ids
  legacy_update_id="$(read_state canaryLegacyUpdateId)"
  counter_total_before="$(unpinned_publish_total canary)"
  echo "  canary counter before the window: $(jq -c . "$WATCHER_DIR/canary.metrics.json"), sum over all labels $counter_total_before"
  [[ "$counter_total_before" -eq 1 ]] || fail "the canary counter sum is $counter_total_before, expected 1 (the publish-v7 legacy publish)"
  record_ledger_end_of_each_host quietWindowStartOffset
  echo "  quiet window: $QUIET_WINDOW_SECONDS seconds"
  sleep "$QUIET_WINDOW_SECONDS"

  echo "  positive control: a 0.7.0 publish after the window. The canary must observe it"
  control_summary="$(publish_and_check canary-gate-control "$CORE_V7_PACKAGE_ID:Wormhole.Core.State:Emitter" "$CORE_V7_PACKAGE_ID")"
  control_update_id="$(jq -r .updateId <<<"$control_summary")"
  append_state_list v7DroppedUpdateIds "$control_update_id"
  await_observation canary "$(jq -r .sequence <<<"$control_summary")" "$(jq -r .payload <<<"$control_summary")" false
  counter_total_after="$(unpinned_publish_total canary)"
  [[ "$counter_total_after" -eq "$counter_total_before" ]] \
    || fail "the canary counter sum changed in the quiet window: $counter_total_before -> $counter_total_after $(cat "$WATCHER_DIR/canary.metrics.json")"
  echo "  PASS: the canary observed the control; its counter sum over all labels stays $counter_total_after"

  for validator_name in "${GUARDIAN_HOSTS[@]}"; do
    control_offset="$(update_offset_on "$validator_name" "$control_update_id")"
    record_host_offset canaryGateOffset "$validator_name" "$control_offset"
    old_package_update_ids="$(scan_publish_update_ids_not_under "$validator_name" "$(read_host_offset startOffset "$validator_name")" "$control_offset" "$CORE_V7_PACKAGE_ID")"
    [[ "$old_package_update_ids" == "$legacy_update_id" ]] \
      || fail "$validator_name: scan ($(read_host_offset startOffset "$validator_name"), $control_offset] finds publishes not under 0.7.0: $(tr '\n' ' ' <<<"$old_package_update_ids"), expected only $legacy_update_id"
    old_package_update_ids="$(scan_publish_update_ids_not_under "$validator_name" "$(read_host_offset quietWindowStartOffset "$validator_name")" "$control_offset" "$CORE_V7_PACKAGE_ID")"
    [[ -z "$old_package_update_ids" ]] || fail "$validator_name: publishes not under 0.7.0 in the quiet window: $(tr '\n' ' ' <<<"$old_package_update_ids")"
    echo "  PASS: $validator_name scan as $WATCHER_USER_ID: one publish not under 0.7.0 since the start offset (the legacy one), none in the quiet window up to offset $control_offset"
  done
  stop_harness canary
  check_harness_reobservation_errors canary
  [[ "$(harness_dropped_update_ids canary "$CORE_V6_PACKAGE_ID")" == "$legacy_update_id" ]] \
    || fail "the canary WARN logs name $(harness_dropped_update_ids canary "$CORE_V6_PACKAGE_ID" | tr '\n' ' '), expected $legacy_update_id"
  echo "  PASS: canary WARN log names only the legacy update $legacy_update_id; the NTT run of step 4 added no 0.6.0 publish"
}

# managers_created_under <ntt-package-id>: burn/mint NttManagers that this package created, whose admin is a LocalNet
# burn/mint admin, as JSON.
managers_created_under() {
  active_contracts "$SUBMITTING_VALIDATOR" "$GG_PARTY" ":Wormhole.Ntt.Manager:NttManager" \
    | jq -c --arg package "$1" '[.[] | select((.templateId | startswith($package + ":")) and (.createArgument.admin | startswith("NttBurnMintAdmin-")))]'
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

# manager_with_admin_key_created_under <ntt-package-id>
manager_with_admin_key_created_under() {
  local manager_index managers admin_label
  managers="$(managers_created_under "$1")"
  for ((manager_index = 0; manager_index < $(jq length <<<"$managers"); manager_index++)); do
    admin_label="$(jq -r --argjson index "$manager_index" '.[$index].createArgument.admin' <<<"$managers" | sed -E 's/^NttBurnMintAdmin-([0-9]+)::.*/\1/')"
    if [[ -f "$NTT_KEY_DIR/burn-mint-admin-$admin_label.key" ]]; then
      jq -c --argjson index "$manager_index" '.[$index]' <<<"$managers"
      return 0
    fi
  done
  fail "no NttManager created by ${1:0:12} with a burn/mint admin key in $NTT_KEY_DIR"
}

step_unvet_v6() {
  echo "### 6. Unvet core 0.6.0 and ntt 0.2.0 together on each participant, immediately before the switch"
  v6_watchers_run || fail "a guardian watcher v6-<host> does not run; run observe-v6 first"
  harness_is_running canary && fail "the canary runs; run the gate step first"
  local validator_name emitter_before emitter_after manager drain_seconds
  for validator_name in "${GUARDIAN_HOSTS[@]}"; do
    change_vetting "$validator_name" Unvet "$NTT_V2_PACKAGE_ID" "$CORE_V6_PACKAGE_ID"
  done
  for validator_name in "${GUARDIAN_HOSTS[@]}"; do
    await_vetting "$validator_name" wormhole-core "$(versions_json "0.7.0 $CORE_V7_PACKAGE_ID")"
    await_vetting "$validator_name" ntt "$(versions_json "0.3.0 $NTT_V3_PACKAGE_ID")"
  done
  record_ledger_end_of_each_host unvetOffset
  record_state unvetTime "$(date -u +%Y-%m-%dT%H:%M:%SZ)"

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
  manager="$(manager_with_admin_key_created_under "$NTT_V2_PACKAGE_ID")"
  record_state nttV2ManagerContractId "$(jq -r .contractId <<<"$manager")"
  if set_paused_on_manager "core-upgrade-unvetted-ntt-v2-$(date +%s)" "$NTT_V2_PACKAGE_ID:Wormhole.Ntt.Manager:NttManager" "$manager" \
      > "$gg_work_dir/unvetted-ntt-v2.out" 2> "$gg_work_dir/unvetted-ntt-v2.err"; then
    fail "an ntt 0.2.0 SetPaused committed after the unvet"
  fi
  expect_unvetted_rejection "$gg_work_dir/unvetted-ntt-v2" "$NTT_V2_PACKAGE_ID" "ntt 0.2.0 SetPaused after the unvet"

  echo "### 6c. Drain gate: wait the synchronizer timeouts. Then a 0.7.0 control publish that each v6-<host> drops"
  drain_seconds="$(drain_seconds_from_synchronizer_parameters)"
  record_state drainSeconds "$drain_seconds"
  echo "  drain wait: $drain_seconds seconds (the three parameters plus a margin of $DRAIN_MARGIN_SECONDS seconds)"
  sleep "$drain_seconds"
  check_input_contract_created_under CoreState "$CORE_V6_PACKAGE_ID"
  check_drain_gate "$(publish_and_check v7-drain-control "$CORE_V7_PACKAGE_ID:Wormhole.Core.State:Emitter" "$CORE_V7_PACKAGE_ID")"
}

# check_drain_gate <control-publish-summary>: the WARN log of the control drop shows that each v6-<host> processed
# its participant up to the control offset. A scan of each participant to that offset finds each 0.6.0 publish in the
# observations of the v6 watcher of that participant.
check_drain_gate() {
  local control_update_id validator_name gate_offset v6_scan_update_ids v6_observed_update_ids missing_update_ids
  control_update_id="$(jq -r .updateId <<<"$1")"
  append_state_list v7DroppedUpdateIds "$control_update_id"
  for validator_name in "${GUARDIAN_HOSTS[@]}"; do
    check_drop_logged "v6-$validator_name" "$control_update_id" "$CORE_V7_PACKAGE_ID"
    gate_offset="$(update_offset_on "$validator_name" "$control_update_id")"
    record_host_offset gateOffset "$validator_name" "$gate_offset"
    v6_scan_update_ids="$(scan_publish_update_ids "$validator_name" "$(read_host_offset startOffset "$validator_name")" "$gate_offset" "$CORE_V6_PACKAGE_ID")"
    [[ -n "$v6_scan_update_ids" ]] || fail "$validator_name: the scan found no 0.6.0 PublishMessage update; publish-v7 makes one"
    v6_observed_update_ids="$(observed_update_ids "v6-$validator_name")"
    missing_update_ids="$(comm -23 <(echo "$v6_scan_update_ids") <(echo "$v6_observed_update_ids"))"
    [[ -z "$missing_update_ids" ]] || fail "$validator_name: 0.6.0 publishes that v6-$validator_name did not observe: $(tr '\n' ' ' <<<"$missing_update_ids")"
    echo "  PASS: $validator_name gate offset $gate_offset: v6-$validator_name dropped the control, and observed each of the $(wc -l <<<"$v6_scan_update_ids" | tr -d ' ') 0.6.0 publishes of the scan ($(read_host_offset startOffset "$validator_name"), $gate_offset]"
  done
}

step_switch_v7() {
  echo "### 7. Coordinated restart: stop v6-<host>, enumerate the dropped updates, start v7-<host> with reobservation requests"
  v6_watchers_run || fail "a guardian watcher v6-<host> does not run; run observe-v6 first"
  stop_watcher_on_each_host v6
  local validator_name switch_offset warn_update_ids scan_update_ids reference_update_ids="" live_summary update_id
  local dropped_count late_old_update_ids
  for validator_name in "${GUARDIAN_HOSTS[@]}"; do
    switch_offset="$(ledger_end_of "$validator_name")"
    record_host_offset switchOffset "$validator_name" "$switch_offset"
    warn_update_ids="$(harness_dropped_update_ids "v6-$validator_name" "$CORE_V7_PACKAGE_ID")"
    scan_update_ids="$(scan_publish_update_ids "$validator_name" "$(read_host_offset startOffset "$validator_name")" "$switch_offset" "$CORE_V7_PACKAGE_ID")"
    [[ -n "$scan_update_ids" ]] || fail "$validator_name: the scan found no 0.7.0 PublishMessage update"
    [[ "$warn_update_ids" == "$scan_update_ids" ]] \
      || fail "$validator_name: the WARN logs and the scan differ: $(diff <(echo "$warn_update_ids") <(echo "$scan_update_ids") | tr '\n' ' ')"
    for update_id in $(jq -r '.v7DroppedUpdateIds[]' "$UPGRADE_STATE"); do
      grep -qxF "$update_id" <<<"$scan_update_ids" || fail "$validator_name: the scan does not list $update_id"
    done
    dropped_count="$(wc -l <<<"$scan_update_ids" | tr -d ' ')"
    [[ "$(jq -c . "$WATCHER_DIR/v6-$validator_name.metrics.json")" == "$(jq -cn --arg v7 "$CORE_V7_PACKAGE_ID" --argjson count "$dropped_count" '{($v7): $count}')" ]] \
      || fail "v6-$validator_name unpinned-publish counts $(cat "$WATCHER_DIR/v6-$validator_name.metrics.json"), expected $dropped_count for 0.7.0"
    late_old_update_ids="$(scan_publish_update_ids_not_under "$validator_name" "$(read_host_offset gateOffset "$validator_name")" "$switch_offset" "$CORE_V7_PACKAGE_ID")"
    [[ -z "$late_old_update_ids" ]] || fail "$validator_name: publishes not under 0.7.0 after the gate offset: $(tr '\n' ' ' <<<"$late_old_update_ids")"
    echo "  PASS: $validator_name ($(read_host_offset startOffset "$validator_name"), $switch_offset]: WARN logs, scan and counter of v6-$validator_name agree on $dropped_count dropped 0.7.0 updates; no publish under another package after the gate offset"
    if [[ -z "$reference_update_ids" ]]; then
      reference_update_ids="$scan_update_ids"
    else
      [[ "$scan_update_ids" == "$reference_update_ids" ]] || fail "$validator_name: the dropped update ids differ from those of ${GUARDIAN_HOSTS[0]}"
    fi
  done
  echo "  PASS: the three participants give the same $dropped_count dropped update ids"
  printf '%s\n' "$reference_update_ids" > "$WATCHER_DIR/dropped-update-ids.txt"

  start_watcher_on_each_host v7 "$CORE_V7_PACKAGE_ID" "$(paste -sd, - <<<"$reference_update_ids")" 900
  live_summary="$(publish_and_check v7-after-switch "$EMITTER_TEMPLATE" "$CORE_V7_PACKAGE_ID")"
  for validator_name in "${GUARDIAN_HOSTS[@]}"; do
    await_observation "v7-$validator_name" "$(jq -r .sequence <<<"$live_summary")" "$(jq -r .payload <<<"$live_summary")" false
  done
  stop_watcher_on_each_host v7
  for validator_name in "${GUARDIAN_HOSTS[@]}"; do
    [[ "$(jq -sr '[.[] | select(.isReobservation) | .updateId] | unique | .[]' "$WATCHER_DIR/v7-$validator_name.jsonl")" == "$reference_update_ids" ]] \
      || fail "v7-$validator_name reobserved $(jq -sc '[.[] | select(.isReobservation) | .updateId] | unique' "$WATCHER_DIR/v7-$validator_name.jsonl"), expected each dropped update"
    [[ "$(jq -c . "$WATCHER_DIR/v7-$validator_name.metrics.json")" == "{}" ]] || fail "v7-$validator_name dropped publishes: $(cat "$WATCHER_DIR/v7-$validator_name.metrics.json")"
  done
  check_watchers_agree v7
  echo "  PASS: each v7-<host> reobserved each of the $dropped_count dropped updates, observed the live publish, and dropped none"
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
  manager="$(manager_with_admin_key_created_under "$NTT_V2_PACKAGE_ID")"
  echo "  input NttManager $(jq -r '.contractId[0:16]' <<<"$manager") created under ntt 0.2.0 $(jq -r '.templateId[0:12]' <<<"$manager")"
  transaction="$(set_paused_on_manager "core-upgrade-ntt-v2-manager-$(date +%s)" "$NTT_MANAGER_TEMPLATE" "$manager")"
  [[ "$(jq -r '[.events[] | .ExercisedEvent? // empty | select(.choice == "SetPaused") | .templateId | split(":")[0]] | unique | .[]' <<<"$transaction")" == "$NTT_V3_PACKAGE_ID" ]] \
    || fail "SetPaused did not execute under ntt 0.3.0"
  [[ "$(jq -r '[.events[] | .CreatedEvent? // empty | select(.templateId | endswith(":Wormhole.Ntt.Manager:NttManager")) | .templateId | split(":")[0]] | .[]' <<<"$transaction")" == "$NTT_V3_PACKAGE_ID" ]] \
    || fail "the successor NttManager was not created under ntt 0.3.0"
  echo "  PASS: SetPaused executed under ntt 0.3.0 ${NTT_V3_PACKAGE_ID:0:12}; the successor NttManager is an ntt 0.3.0 contract"

  echo "### 9e. ntt_e2e.sh with ntt 0.3.0 and a watcher pinned to 0.7.0"
  run_ntt_e2e_under_v7 false
}

step_rollback() {
  echo "### 10a. Recovery without a re-vet: 0.6.0 is unvetted. A watcher pinned to 0.6.0 reobserves an old 0.6.0 update"
  local validator_name legacy_update_id summary manager transaction
  for validator_name in "${GUARDIAN_HOSTS[@]}"; do
    await_vetting "$validator_name" wormhole-core "$(versions_json "0.7.0 $CORE_V7_PACKAGE_ID")"
    await_vetting "$validator_name" ntt "$(versions_json "0.3.0 $NTT_V3_PACKAGE_ID")"
  done
  [[ -x "$(harness_binary)" ]] || build_harness
  legacy_update_id="$(read_state canaryLegacyUpdateId)"
  check_publish_package_on_each_host "$legacy_update_id" "$CORE_V6_PACKAGE_ID"
  if harness_is_running v6-recovery; then stop_harness v6-recovery; fi
  run_harness v6-recovery "$CORE_V6_PACKAGE_ID" "$legacy_update_id" 300
  await_observation_of_update v6-recovery "$legacy_update_id" true
  stop_harness v6-recovery
  check_harness_reobservation_errors v6-recovery
  echo "  PASS: a watcher pinned to 0.6.0 reobserved the 0.6.0 update $legacy_update_id while no participant vets 0.6.0"

  echo "### 10b. Rollback: re-vet core 0.6.0 and ntt 0.2.0 together on each participant"
  for validator_name in "${GUARDIAN_HOSTS[@]}"; do
    change_vetting "$validator_name" Vet "$NTT_V2_PACKAGE_ID" "$CORE_V6_PACKAGE_ID"
  done
  for validator_name in "${GUARDIAN_HOSTS[@]}"; do
    await_vetting "$validator_name" wormhole-core "$(versions_json "0.6.0 $CORE_V6_PACKAGE_ID" "0.7.0 $CORE_V7_PACKAGE_ID")"
    await_vetting "$validator_name" ntt "$(versions_json "0.2.0 $NTT_V2_PACKAGE_ID" "0.3.0 $NTT_V3_PACKAGE_ID")"
  done
  start_watcher_on_each_host rollback-v6 "$CORE_V6_PACKAGE_ID" "" 900

  echo "### 10c. A 0.6.0 PublishMessage on the Emitter that 0.7.0 created"
  check_input_contract_created_under Emitter "$CORE_V7_PACKAGE_ID"
  summary="$(publish_and_check v6-rollback "$CORE_V6_PACKAGE_ID:Wormhole.Core.State:Emitter" "$CORE_V6_PACKAGE_ID")"
  check_publish_package_on_each_host "$(jq -r .updateId <<<"$summary")" "$CORE_V6_PACKAGE_ID"
  check_input_contract_created_under Emitter "$CORE_V6_PACKAGE_ID"
  for validator_name in "${GUARDIAN_HOSTS[@]}"; do
    await_observation "rollback-v6-$validator_name" "$(jq -r .sequence <<<"$summary")" "$(jq -r .payload <<<"$summary")" false
  done
  echo "  PASS: 0.6.0 ran PublishMessage on the 0.7.0 Emitter; each rollback-v6-<host> observed it"

  echo "### 10d. An ntt 0.2.0 SetPaused on an NttManager that ntt 0.3.0 created"
  manager="$(manager_with_admin_key_created_under "$NTT_V3_PACKAGE_ID")"
  echo "  input NttManager $(jq -r '.contractId[0:16]' <<<"$manager") created under ntt 0.3.0 $(jq -r '.templateId[0:12]' <<<"$manager")"
  transaction="$(set_paused_on_manager "core-upgrade-rollback-ntt-v2-$(date +%s)" "$NTT_V2_PACKAGE_ID:Wormhole.Ntt.Manager:NttManager" "$manager")"
  [[ "$(jq -r '[.events[] | .ExercisedEvent? // empty | select(.choice == "SetPaused") | .templateId | split(":")[0]] | unique | .[]' <<<"$transaction")" == "$NTT_V2_PACKAGE_ID" ]] \
    || fail "SetPaused did not execute under ntt 0.2.0"
  [[ "$(jq -r '[.events[] | .CreatedEvent? // empty | select(.templateId | endswith(":Wormhole.Ntt.Manager:NttManager")) | .templateId | split(":")[0]] | .[]' <<<"$transaction")" == "$NTT_V2_PACKAGE_ID" ]] \
    || fail "the successor NttManager was not created under ntt 0.2.0"
  echo "  PASS: ntt 0.2.0 ran SetPaused on the ntt 0.3.0 NttManager; the successor is an ntt 0.2.0 contract"
  stop_watcher_on_each_host rollback-v6
  check_watchers_agree rollback-v6
}

step_restore() {
  echo "### 11. Restore the final state: unvet core 0.6.0 and ntt 0.2.0 together on each participant"
  local validator_name
  for validator_name in "${GUARDIAN_HOSTS[@]}"; do
    change_vetting "$validator_name" Unvet "$NTT_V2_PACKAGE_ID" "$CORE_V6_PACKAGE_ID"
  done
  for validator_name in "${GUARDIAN_HOSTS[@]}"; do
    await_vetting "$validator_name" wormhole-core "$(versions_json "0.7.0 $CORE_V7_PACKAGE_ID")"
    await_vetting "$validator_name" ntt "$(versions_json "0.3.0 $NTT_V3_PACKAGE_ID")"
  done
  if publish_on_operator_emitter "core-upgrade-restored-v6-$(date +%s)" "$CORE_V6_PACKAGE_ID:Wormhole.Core.State:Emitter" \
      1 "$(hex_of "core upgrade after the restore")" > "$gg_work_dir/restored-v6.out" 2> "$gg_work_dir/restored-v6.err"; then
    fail "a 0.6.0 PublishMessage committed after the restore"
  fi
  expect_unvetted_rejection "$gg_work_dir/restored-v6" "$CORE_V6_PACKAGE_ID" "0.6.0 PublishMessage after the restore"
  echo "  the 0.6.0 Emitter of the rollback works under 0.7.0:"
  publish_and_check after-restore "$EMITTER_TEMPLATE" "$CORE_V7_PACKAGE_ID" > /dev/null
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
  rollback) step_rollback ;;
  restore) step_restore ;;
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
    step_rollback
    step_restore
    ;;
  *)
    echo "usage: core_upgrade_rehearsal.sh prepare|observe-v6|vet-v7|publish-v7|ntt|gate|unvet-v6|switch-v7|enforce-v7|post-unvet|rollback|restore|all" >&2
    exit 2
    ;;
esac
echo "core_upgrade_rehearsal.sh ${1}: PASS"
