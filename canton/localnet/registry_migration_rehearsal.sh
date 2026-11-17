#!/usr/bin/env bash
# shellcheck disable=SC2154 # gg_work_dir comes from the sourced gg_lib.sh
# Rehearse the wormhole-core 0.7.0 to 0.8.0 EmitterRegistry migration (canton/A14_EMITTER_ID_UNIQUENESS.md) on LocalNet.
# Steps (run one, or "all" in order):
#   prepare        Each host vets only core 0.7.0 and ntt 0.3.0. Record the start offsets. Start v7-<host>, pinned to 0.7.0.
#   vet-v8         Each host uploads and vets core 0.8.0, then the ntt 0.4.0 that is rebuilt on core 0.8.0.
#   cutover        Start the canary, pinned to 0.8.0. A 0.8.0 publish: the canary observes it, each v7-<host> drops it.
#                  A RegisterEmitter by package name on the 0.7.0 registry fails. The operator plants a 0.7.0 registry
#                  with nextId at the predicted firstId (the attack of the review), then archives the real 0.7.0 one.
#   switch-pin     Stop v7-<host>. The WARN logs give the dropped update ids. Start v8-<host>, pinned to 0.8.0, with
#                  reobservation requests for them.
#   unvet-v7       Each host unvets core 0.7.0 and ntt 0.3.0 in one topology transaction. A 0.7.0 publish and a 0.7.0
#                  RegisterEmitter on the planted registry fail at prepare. A 0.8.0 RegisterEmitter on it fails.
#   scan           emitter_id_scan.sh on each host as the CanReadAs(guardianGovernance) user. All hosts agree on firstId.
#   ceremony       The operator alone, and the operator with one owner key, cannot create the 0.8.0 registry. The
#                  operator with two of three owner keys creates it with nextId = firstId.
#   register       A new external party registers on the 0.8.0 registry and publishes. Each v8-<host> observes the
#                  message with emitterId firstId. The scan then passes with one active registry.
# Usage: registry_migration_rehearsal.sh <step>|all
# Environment: LOCALNET_SCRIPTS_DIR is the canton/localnet directory that holds ntt_lib.sh and watcher_harness_lib.sh
# (default: this directory). NTT_V8_DAR is the ntt 0.4.0 DAR built on core 0.8.0.
set -euo pipefail

REHEARSAL_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
readonly REHEARSAL_DIR
U2_REPO_ROOT="$(cd "$REHEARSAL_DIR/../.." && pwd)"
readonly U2_REPO_ROOT
LOCALNET_SCRIPTS_DIR="${LOCALNET_SCRIPTS_DIR:-$REHEARSAL_DIR}"
[[ -f "$LOCALNET_SCRIPTS_DIR/ntt_lib.sh" ]] || { echo "no ntt_lib.sh in $LOCALNET_SCRIPTS_DIR; set LOCALNET_SCRIPTS_DIR" >&2; exit 2; }
# shellcheck source=/dev/null
source "$LOCALNET_SCRIPTS_DIR/ntt_lib.sh"
echo "Rehearsal script commit $(git -C "$U2_REPO_ROOT" rev-parse --short HEAD), LocalNet scripts commit $(git -C "$REPO_ROOT" rev-parse --short HEAD)"

readonly CORE_V7_PACKAGE_ID="7a22a1c4026b2a037b021063c4ff2bd5506be2e748aa9de8b7a6057c90c356eb"
readonly CORE_V8_PACKAGE_ID="a812f155092098baeac5e493f509d916124fc216858d828e0cc70027288fdb10"
readonly NTT_V3_PACKAGE_ID="291fcf920b971d50956845c2690723fb321f0a5b141a44caa5a16910f6ffa1a6"
readonly NTT_V8_PACKAGE_ID="59093de8bcfc553e82c985303f7237cf7cd6d76235c1ca05bac016c443c0592b"
readonly CORE_V8_DAR="$U2_REPO_ROOT/canton/core/.daml/dist/wormhole-core-0.8.0.dar"
readonly NTT_V8_DAR="${NTT_V8_DAR:?set NTT_V8_DAR to the ntt 0.4.0 DAR built on core 0.8.0}"
readonly SCAN_SCRIPT="$U2_REPO_ROOT/canton/tools/emitter_id_scan.sh"
readonly DPM_BIN_DIR="${DPM_BIN_DIR:-$HOME/.dpm/bin}"
readonly GUARDIAN_HOSTS=(app-provider sv app-user)
readonly MIGRATION_STATE="$LOCALNET_STATE_DIR/registry-migration.json"
readonly WATCHER_DIR="$LOCALNET_STATE_DIR/registry-migration-watcher"
readonly OBSERVATION_WAIT_SECONDS=60
readonly WATCHER_MAX_SECONDS=1800
readonly UNVETTED_ERROR_CODE="INVALID_PRESCRIBED_SYNCHRONIZER_ID"
readonly REGISTRY_SUFFIX=":Wormhole.Core.State:EmitterRegistry"
readonly EMITTER_SUFFIX=":Wormhole.Core.State:Emitter"
read -r -a CEREMONY_OWNERS <<<"${CEREMONY_OWNERS:-owner1 owner2}"

# shellcheck source=/dev/null
source "$LOCALNET_SCRIPTS_DIR/watcher_harness_lib.sh"
mkdir -p "$WATCHER_DIR"
[[ -f "$MIGRATION_STATE" ]] || echo '{}' > "$MIGRATION_STATE"

fail() {
  echo "FAIL: $*" >&2
  exit 1
}

record_state() {
  local updated
  updated="$(jq --arg key "$1" --argjson value "$2" '.[$key] = $value' "$MIGRATION_STATE")"
  printf '%s\n' "$updated" > "$MIGRATION_STATE"
}

read_state() {
  jq -er --arg key "$1" '.[$key]' "$MIGRATION_STATE"
}

dar_main_package_id() {
  PATH="$DPM_BIN_DIR:$PATH" dpm damlc inspect-dar --json "$1" 2>/dev/null | jq -er .main_package_id
}

participant_id_of() {
  curl -sSf "$(json_api_url "$1")/v2/parties/participant-id" -H "@$ADMIN_AUTH_HEADER" | jq -er .participantId
}

ledger_end_of() {
  curl -sSf "$(json_api_url "$1")/v2/state/ledger-end" -H "@$SCRIPT_AUTH_HEADER" | jq -er .offset
}

# vetted_versions <validator> <package-name>: sorted JSON array of "<version> <package id>".
vetted_versions() {
  curl -sSf -X POST "$(json_api_url "$1")/v2/package-vetting/list" -H "@$ADMIN_AUTH_HEADER" \
    -H 'Content-Type: application/json' \
    -d "$(jq -n --arg pid "$(participant_id_of "$1")" --arg name "$2" \
      '{packageMetadataFilter: {packageNamePrefixes: [$name]}, topologyStateFilter: {participantIds: [$pid]}}')" \
    | jq -c --arg name "$2" '[.vettedPackages[]?.packages[] | select(.packageName == $name) | "\(.packageVersion) \(.packageId)"] | unique'
}

versions_json() {
  jq -cn '$ARGS.positional | sort' --args "$@"
}

# await_vetting <validator> <package-name> <expected-json>
await_vetting() {
  local attempt
  for attempt in $(seq 1 60); do
    if [[ "$attempt" -gt 1 ]]; then sleep 1; fi
    if [[ "$(vetted_versions "$1" "$2")" == "$3" ]]; then
      echo "  $1 vets $2 $(jq -c 'map(.[0:18])' <<<"$3")"
      return 0
    fi
  done
  fail "$1 vets $2 $(vetted_versions "$1" "$2"), expected $3"
}

upload_and_vet() {
  local http_status
  http_status="$(curl -sS -o "$gg_work_dir/upload.out" -w '%{http_code}' -X POST \
    "$(json_api_url "$1")/v2/dars?vetAllPackages=true" \
    -H "@$ADMIN_AUTH_HEADER" -H 'Content-Type: application/octet-stream' --data-binary "@$2")"
  [[ "$http_status" == "200" ]] || fail "upload of $(basename "$2") to $1: HTTP $http_status $(cat "$gg_work_dir/upload.out")"
  echo "  $1: uploaded and vetted $(basename "$2") (HTTP 200)"
}

# change_vetting <validator> <Vet|Unvet> <package-id>...: one topology transaction for all named packages.
change_vetting() {
  local validator_name="$1" operation="$2"
  shift 2
  curl -sSf -X POST "$(json_api_url "$validator_name")/v2/package-vetting/update" -H "@$ADMIN_AUTH_HEADER" \
    -H 'Content-Type: application/json' \
    -d "$(jq -n --arg operation "$operation" --arg sync "$LOGICAL_SYNCHRONIZER_ID" '{
      changes: [{operation: {($operation): {value: {packages: [$ARGS.positional[] | {packageId: .}]}}}}],
      dryRun: false, synchronizerId: $sync}' --args "$@")" > /dev/null
  echo "  $validator_name: $operation $(printf '%.12s ' "$@")in one topology transaction"
}

hex_of() {
  printf '%s' "$1" | od -An -tx1 | tr -d ' \n'
}

# publish_on_operator_emitter <label> <template-id>: PublishMessage on the Operator Emitter; prints the summary JSON.
publish_on_operator_emitter() {
  local payload_hex transaction
  payload_hex="$(hex_of "registry migration $(date +%s) $1")"
  transaction="$(submit_interactive "registry-migration-$1-$(date +%s)" "$(jq -n --arg op "$OPERATOR_PARTY" '[$op]')" \
    "$(exercise_command "$2" "$(core_contract_id emitter)" PublishMessage \
      "$(jq -n --arg op "$OPERATOR_PARTY" --arg cs "$(core_contract_id coreState)" --arg payload "$payload_hex" \
        '{nonce: "1", payload: $payload, consistencyLevel: "0", coreStateCid: $cs, payer: $op, feeAllocation: null}')")" \
    "$(operator_signers_json)")"
  publish_summary "$transaction"
}

# publish_summary <transaction-json>: update id, executing package, sequence, payload and emitter of PublishMessage.
publish_summary() {
  jq -ec '. as $transaction | [.events[] | .ExercisedEvent? // empty | select(.choice == "PublishMessage")]
    | if length != 1 then error("expected one PublishMessage event") else .[0] end
    | {updateId: $transaction.updateId, executingPackageId: (.templateId | split(":")[0]),
       sequence: .exerciseResult._2.sequence, payload: .exerciseResult._2.payload,
       emitterId: .exerciseResult._2.emitterId, owner: .exerciseResult._2.owner}' <<<"$1"
}

# await_observation <harness-name> <update-id> <reobservation true|false>: prints the observation JSON.
await_observation() {
  local waited_seconds
  for ((waited_seconds = 0; waited_seconds < OBSERVATION_WAIT_SECONDS; waited_seconds++)); do
    if [[ -s "$WATCHER_DIR/$1.jsonl" ]] && jq -se --arg id "$2" --argjson reobserved "$3" \
        'any(.[]; .updateId == $id and .isReobservation == $reobserved)' "$WATCHER_DIR/$1.jsonl" > /dev/null; then
      jq -sc --arg id "$2" --argjson reobserved "$3" \
        '[.[] | select(.updateId == $id and .isReobservation == $reobserved)][0]' "$WATCHER_DIR/$1.jsonl"
      return 0
    fi
    sleep 1
  done
  fail "watcher $1 has no observation of update $2 (reobservation $3) after ${OBSERVATION_WAIT_SECONDS}s"
}

# await_drop_logged <harness-name> <update-id> <executing-package-id>
await_drop_logged() {
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

# expect_unvetted_rejection <error-file> <package-id> <description>: the error has the code and names each host.
expect_unvetted_rejection() {
  local naming_count
  grep -qF "$UNVETTED_ERROR_CODE" "$1" || { cat "$1" >&2; fail "$3: no $UNVETTED_ERROR_CODE"; }
  naming_count="$(grep -oF "has not vetted ${2:0:12}" "$1" | wc -l | tr -d ' ')"
  [[ "$naming_count" -eq "${#GUARDIAN_HOSTS[@]}" ]] \
    || { cat "$1" >&2; fail "$3: 'has not vetted ${2:0:12}' names $naming_count participants, expected ${#GUARDIAN_HOSTS[@]}"; }
  echo "  PASS: $3 rejected ($UNVETTED_ERROR_CODE): each of the ${#GUARDIAN_HOSTS[@]} participants 'has not vetted ${2:0:12}'"
}

# expect_rejection_with <error-file> <expected-substring> <description>: case-insensitive match.
expect_rejection_with() {
  grep -qiF -- "$2" "$1" || { cat "$1" >&2; fail "$3: the error has no '$2'"; }
  echo "  PASS: $3 rejected: $(grep -oE '[A-Z_]{8,}' "$1" | sort -u | tr '\n' ' ')"
}

operator_registries() {
  active_contracts "$SUBMITTING_VALIDATOR" "$OPERATOR_PARTY" "$REGISTRY_SUFFIX"
}

# register_emitter_command <template-id> <registry-contract-id> <requester>
register_emitter_command() {
  exercise_command "$1" "$2" RegisterEmitter \
    "$(jq -n --arg requester "$3" '{requester: $requester, payer: $requester, coreStateCid: null, feeAllocation: null}')"
}

migration_owner_signers_json() {
  external_party_signers_json "$(read_state migrationOwner | jq -r .partyId)" \
    "$(read_state migrationOwner | jq -r .keyFile)" "$(read_state migrationOwner | jq -r .fingerprint)"
}

start_watcher_on_each_host() {
  local validator_name
  for validator_name in "${GUARDIAN_HOSTS[@]}"; do
    run_harness_on "$validator_name" "$1-$validator_name" "$2" "$3" "$WATCHER_MAX_SECONDS"
  done
}

stop_watcher_on_each_host() {
  local validator_name
  for validator_name in "${GUARDIAN_HOSTS[@]}"; do
    stop_harness "$1-$validator_name"
    check_harness_reobservation_errors "$1-$validator_name"
  done
}

step_prepare() {
  echo "### 1. Start state: core 0.7.0 and ntt 0.3.0 only. Start v7-<host>, pinned to 0.7.0"
  local validator_name summary
  [[ -f "$CORE_V8_DAR" ]] || fail "$CORE_V8_DAR not found; run 'dpm build --all' in canton/"
  [[ "$(dar_main_package_id "$CORE_V8_DAR")" == "$CORE_V8_PACKAGE_ID" ]] || fail "$CORE_V8_DAR is not $CORE_V8_PACKAGE_ID"
  [[ "$(dar_main_package_id "$NTT_V8_DAR")" == "$NTT_V8_PACKAGE_ID" ]] || fail "$NTT_V8_DAR is not $NTT_V8_PACKAGE_ID"
  echo "  DAR package ids: core 0.8.0 ${CORE_V8_PACKAGE_ID:0:12}, ntt 0.4.0 ${NTT_V8_PACKAGE_ID:0:12}"
  for validator_name in "${GUARDIAN_HOSTS[@]}"; do
    await_vetting "$validator_name" wormhole-core "$(versions_json "0.7.0 $CORE_V7_PACKAGE_ID")"
    await_vetting "$validator_name" ntt "$(versions_json "0.3.0 $NTT_V3_PACKAGE_ID")"
    record_state "startOffset-$validator_name" "\"$(ledger_end_of "$validator_name")\""
  done
  build_harness
  start_watcher_on_each_host v7 "$CORE_V7_PACKAGE_ID" ""
  summary="$(publish_on_operator_emitter v7-control "$EMITTER_TEMPLATE")"
  [[ "$(jq -r .executingPackageId <<<"$summary")" == "$CORE_V7_PACKAGE_ID" ]] || fail "the control publish did not run under 0.7.0: $summary"
  for validator_name in "${GUARDIAN_HOSTS[@]}"; do
    echo "  v7-$validator_name observed: $(await_observation "v7-$validator_name" "$(jq -r .updateId <<<"$summary")" false | jq -c '{sequence, updateId}')"
  done
}

step_vet_v8() {
  echo "### 2. Upload and vet core 0.8.0, then ntt 0.4.0 (NTT-first rule), on each host"
  local validator_name
  for validator_name in "${GUARDIAN_HOSTS[@]}"; do
    upload_and_vet "$validator_name" "$CORE_V8_DAR"
    upload_and_vet "$validator_name" "$NTT_V8_DAR"
    await_vetting "$validator_name" wormhole-core "$(versions_json "0.7.0 $CORE_V7_PACKAGE_ID" "0.8.0 $CORE_V8_PACKAGE_ID")"
    await_vetting "$validator_name" ntt "$(versions_json "0.3.0 $NTT_V3_PACKAGE_ID" "0.4.0 $NTT_V8_PACKAGE_ID")"
  done
}

step_cutover() {
  echo "### 3. Integrator cutover: canary on 0.8.0, a 0.8.0 publish, registrations stop, a planted 0.7.0 registry"
  local summary validator_name owner_json real_registry predicted_first_id planted_transaction planted_registry
  run_harness_on "$SUBMITTING_VALIDATOR" canary "$CORE_V8_PACKAGE_ID" "" "$WATCHER_MAX_SECONDS"
  summary="$(publish_on_operator_emitter v8-cutover "$CORE_V8_PACKAGE_ID$EMITTER_SUFFIX")"
  [[ "$(jq -r .executingPackageId <<<"$summary")" == "$CORE_V8_PACKAGE_ID" ]] || fail "the cutover publish did not run under 0.8.0: $summary"
  echo "  published under 0.8.0: $(jq -c '{sequence, updateId}' <<<"$summary")"
  echo "  canary observed: $(await_observation canary "$(jq -r .updateId <<<"$summary")" false | jq -c '{sequence, emitterAddress}')"
  for validator_name in "${GUARDIAN_HOSTS[@]}"; do
    await_drop_logged "v7-$validator_name" "$(jq -r .updateId <<<"$summary")" "$CORE_V8_PACKAGE_ID"
  done
  record_state droppedUpdateId "\"$(jq -r .updateId <<<"$summary")\""

  owner_json="$(allocate_external_party "$SUBMITTING_VALIDATOR" "RegistryMigrationOwner-$(date +%s)" "$NTT_KEY_DIR/registry-migration-owner-$(date +%s)")"
  record_state migrationOwner "$owner_json"
  echo "  migration owner: $(jq -r .partyId <<<"$owner_json")"
  real_registry="$(operator_registries | jq -ec 'if length == 1 then .[0] else error("expected one EmitterRegistry, found \(length)") end')"
  [[ "$(jq -r '.templateId | split(":")[0]' <<<"$real_registry")" == "$CORE_V7_PACKAGE_ID" ]] || fail "the active registry is not a 0.7.0 contract"
  echo "  real 0.7.0 registry: nextId $(jq -r .createArgument.nextId <<<"$real_registry"), signatories $(jq -c '.signatories | map(.[0:18])' <<<"$real_registry")"
  if submit_interactive "registry-migration-register-by-name-$(date +%s)" "$(jq -n --arg p "$(jq -r .partyId <<<"$owner_json")" '[$p]')" \
      "$(register_emitter_command "$EMITTER_REGISTRY_TEMPLATE" "$(jq -r .contractId <<<"$real_registry")" "$(jq -r .partyId <<<"$owner_json")")" \
      "$(migration_owner_signers_json)" "[$(operator_disclosure "$REGISTRY_SUFFIX" "$(jq -r .contractId <<<"$real_registry")")]" \
      > "$gg_work_dir/register-by-name.out" 2> "$gg_work_dir/register-by-name.err"; then
    fail "RegisterEmitter by package name on the 0.7.0 registry committed while 0.8.0 is vetted"
  fi
  echo "  PASS: RegisterEmitter by package name on the 0.7.0 registry rejected: $(grep -oE '[A-Z_]{8,}' "$gg_work_dir/register-by-name.err" | sort -u | tr '\n' ' ')"
  grep -oE '(signatories|Signatories|signatory)[^"]{0,160}' "$gg_work_dir/register-by-name.err" | head -n 2 | sed 's/^/    /' || true

  predicted_first_id="$(jq -r .createArgument.nextId <<<"$real_registry")"
  planted_transaction="$(submit_interactive "registry-migration-plant-v7-$(date +%s)" "$(jq -n --arg op "$OPERATOR_PARTY" '[$op]')" \
    "$(create_command "$CORE_V7_PACKAGE_ID$REGISTRY_SUFFIX" "$(jq -n --arg op "$OPERATOR_PARTY" --arg gg "$GG_PARTY" --arg next "$predicted_first_id" \
      '{operator: $op, guardianGovernance: $gg, registrationFee: "0", nextId: $next}')")" "$(operator_signers_json)")"
  planted_registry="$(created_contract_id "$planted_transaction" "$REGISTRY_SUFFIX")"
  record_state plantedRegistry "\"$planted_registry\""
  echo "  operator alone planted a 0.7.0 EmitterRegistry with nextId $predicted_first_id: ${planted_registry:0:24}"
  submit_interactive "registry-migration-archive-v7-$(date +%s)" "$(jq -n --arg op "$OPERATOR_PARTY" '[$op]')" \
    "$(exercise_command "$CORE_V7_PACKAGE_ID$REGISTRY_SUFFIX" "$(jq -r .contractId <<<"$real_registry")" Archive '{}')" \
    "$(operator_signers_json)" > /dev/null
  echo "  operator archived the real 0.7.0 registry; active operator registries: $(operator_registries | jq -c 'map({nextId: .createArgument.nextId, package: (.templateId[0:12])})')"
}

step_switch_pin() {
  echo "### 4. Watcher pin switch: stop v7-<host>, start v8-<host> with reobservation of the dropped updates"
  local validator_name dropped_update_id observation
  dropped_update_id="$(read_state droppedUpdateId)"
  stop_watcher_on_each_host v7
  stop_harness canary
  for validator_name in "${GUARDIAN_HOSTS[@]}"; do
    [[ "$(harness_dropped_update_ids "v7-$validator_name" "$CORE_V8_PACKAGE_ID")" == "$dropped_update_id" ]] \
      || fail "v7-$validator_name WARN logs list $(harness_dropped_update_ids "v7-$validator_name" "$CORE_V8_PACKAGE_ID" | tr '\n' ' '), expected $dropped_update_id"
    echo "  v7-$validator_name: dropped 0.8.0 updates per WARN log: $dropped_update_id; counter $(jq -c . "$WATCHER_DIR/v7-$validator_name.metrics.json")"
  done
  start_watcher_on_each_host v8 "$CORE_V8_PACKAGE_ID" "$dropped_update_id"
  for validator_name in "${GUARDIAN_HOSTS[@]}"; do
    observation="$(await_observation "v8-$validator_name" "$dropped_update_id" true)"
    echo "  PASS: v8-$validator_name reobserved $dropped_update_id: $(jq -c '{sequence, emitterAddress}' <<<"$observation")"
  done
}

step_unvet_v7() {
  echo "### 5. Unvet core 0.7.0 and ntt 0.3.0 on each host; 0.7.0 submissions fail"
  local validator_name planted_registry owner_party
  for validator_name in "${GUARDIAN_HOSTS[@]}"; do
    change_vetting "$validator_name" Unvet "$CORE_V7_PACKAGE_ID" "$NTT_V3_PACKAGE_ID"
  done
  for validator_name in "${GUARDIAN_HOSTS[@]}"; do
    await_vetting "$validator_name" wormhole-core "$(versions_json "0.8.0 $CORE_V8_PACKAGE_ID")"
    await_vetting "$validator_name" ntt "$(versions_json "0.4.0 $NTT_V8_PACKAGE_ID")"
    record_state "unvetOffset-$validator_name" "\"$(ledger_end_of "$validator_name")\""
  done
  if publish_on_operator_emitter v7-after-unvet "$CORE_V7_PACKAGE_ID$EMITTER_SUFFIX" > "$gg_work_dir/publish-v7.out" 2> "$gg_work_dir/publish-v7.err"; then
    fail "a 0.7.0 publish committed after the unvet"
  fi
  expect_unvetted_rejection "$gg_work_dir/publish-v7.err" "$CORE_V7_PACKAGE_ID" "0.7.0 PublishMessage after the unvet"

  planted_registry="$(read_state plantedRegistry)"
  owner_party="$(read_state migrationOwner | jq -r .partyId)"
  if submit_interactive "registry-migration-register-v7-$(date +%s)" "$(jq -n --arg p "$owner_party" '[$p]')" \
      "$(register_emitter_command "$CORE_V7_PACKAGE_ID$REGISTRY_SUFFIX" "$planted_registry" "$owner_party")" \
      "$(migration_owner_signers_json)" "[$(operator_disclosure "$REGISTRY_SUFFIX" "$planted_registry")]" \
      > "$gg_work_dir/register-v7.out" 2> "$gg_work_dir/register-v7.err"; then
    fail "a 0.7.0 RegisterEmitter on the planted registry committed after the unvet"
  fi
  expect_unvetted_rejection "$gg_work_dir/register-v7.err" "$CORE_V7_PACKAGE_ID" "0.7.0 RegisterEmitter on the planted registry after the unvet"
  if submit_interactive "registry-migration-register-v8-planted-$(date +%s)" "$(jq -n --arg p "$owner_party" '[$p]')" \
      "$(register_emitter_command "$CORE_V8_PACKAGE_ID$REGISTRY_SUFFIX" "$planted_registry" "$owner_party")" \
      "$(migration_owner_signers_json)" "[$(operator_disclosure "$REGISTRY_SUFFIX" "$planted_registry")]" \
      > "$gg_work_dir/register-v8-planted.out" 2> "$gg_work_dir/register-v8-planted.err"; then
    fail "a 0.8.0 RegisterEmitter on the planted 0.7.0 registry committed"
  fi
  echo "  PASS: 0.8.0 RegisterEmitter on the planted 0.7.0 registry rejected: $(grep -oE '[A-Z_]{8,}' "$gg_work_dir/register-v8-planted.err" | sort -u | tr '\n' ' ')"
  grep -oE '(signatories|Signatories|signatory)[^"]{0,160}' "$gg_work_dir/register-v8-planted.err" | head -n 2 | sed 's/^/    /' || true
}

# run_scan_on <validator> <expected-active-registries>: prints the summary JSON.
run_scan_on() {
  local scan_token_file="$gg_work_dir/scan-$1.jwt"
  ensure_watcher_user "$1" >&2
  (umask 077 && "$LOCALNET_SCRIPTS_DIR/token.sh" "$WATCHER_USER_ID" > "$scan_token_file")
  EXPECT_ACTIVE_REGISTRIES="$2" "$SCAN_SCRIPT" "$(json_api_url "$1")" "$scan_token_file" "$GG_PARTY" "$OPERATOR_PARTY" 0
}

step_scan() {
  echo "### 6. Scan as CanReadAs(guardianGovernance) from genesis on each host"
  local validator_name summary first_id="" host_first_id
  for validator_name in "${GUARDIAN_HOSTS[@]}"; do
    summary="$(run_scan_on "$validator_name" 0)" || fail "the scan on $validator_name failed"
    echo "  $validator_name: $summary"
    host_first_id="$(jq -r .firstId <<<"$summary")"
    if [[ -z "$first_id" ]]; then first_id="$host_first_id"; fi
    [[ "$host_first_id" == "$first_id" ]] || fail "$validator_name gives firstId $host_first_id, ${GUARDIAN_HOSTS[0]} gives $first_id"
  done
  record_state firstId "\"$first_id\""
  echo "  PASS: the ${#GUARDIAN_HOSTS[@]} hosts agree on firstId $first_id"
}

# registry_create_command <next-id>: create of a 0.8.0 EmitterRegistry for Operator and guardianGovernance.
registry_create_command() {
  create_command "$CORE_V8_PACKAGE_ID$REGISTRY_SUFFIX" "$(jq -n --arg op "$OPERATOR_PARTY" --arg gg "$GG_PARTY" --arg next "$1" \
    '{operator: $op, guardianGovernance: $gg, registrationFee: "0", nextId: $next}')"
}

# operator_and_owner_signers_json <owner-name>...: Operator signature and these guardianGovernance owner keys.
operator_and_owner_signers_json() {
  jq -n --argjson operator "$(operator_signers_json)" --argjson owners "$(gg_signers_json "$@")" '$operator * $owners'
}

step_ceremony() {
  echo "### 7. Guardian ceremony: create the 0.8.0 EmitterRegistry with nextId = firstId"
  local first_id act_as transaction registry_cid registry_argument
  first_id="$(read_state firstId)"
  act_as="$(jq -n --arg op "$OPERATOR_PARTY" --arg gg "$GG_PARTY" '[$op, $gg]')"
  if submit_interactive "registry-migration-ceremony-operator-alone-$(date +%s)" "$(jq -n --arg op "$OPERATOR_PARTY" '[$op]')" \
      "$(registry_create_command "$first_id")" "$(operator_signers_json)" \
      > "$gg_work_dir/ceremony-operator.out" 2> "$gg_work_dir/ceremony-operator.err"; then
    fail "the operator alone created a 0.8.0 EmitterRegistry"
  fi
  expect_rejection_with "$gg_work_dir/ceremony-operator.err" "authoriz" "operator-alone 0.8.0 registry create"
  if submit_interactive "registry-migration-ceremony-one-owner-$(date +%s)" "$act_as" "$(registry_create_command "$first_id")" \
      "$(operator_and_owner_signers_json "${CEREMONY_OWNERS[0]}")" \
      > "$gg_work_dir/ceremony-one.out" 2> "$gg_work_dir/ceremony-one.err"; then
    fail "one guardianGovernance owner key created a 0.8.0 EmitterRegistry"
  fi
  expect_rejection_with "$gg_work_dir/ceremony-one.err" "FAILED_TO_EXECUTE_TRANSACTION" "operator and one owner key 0.8.0 registry create"
  transaction="$(submit_interactive "registry-migration-ceremony-$(date +%s)" "$act_as" "$(registry_create_command "$first_id")" \
    "$(operator_and_owner_signers_json "${CEREMONY_OWNERS[@]}")")"
  registry_cid="$(created_contract_id "$transaction" "$REGISTRY_SUFFIX")"
  registry_argument="$(created_contract_argument "$transaction" "$REGISTRY_SUFFIX")"
  [[ "$(jq -r .nextId <<<"$registry_argument")" == "$first_id" ]] || fail "the new registry has nextId $(jq -r .nextId <<<"$registry_argument")"
  record_state migratedRegistry "\"$registry_cid\""
  echo "  PASS: operator + ${CEREMONY_OWNERS[*]} created the 0.8.0 EmitterRegistry ${registry_cid:0:24} with nextId $first_id (update $(jq -r .updateId <<<"$transaction"))"
}

step_register() {
  echo "### 8. Register a new emitter on the 0.8.0 registry and publish; each v8-<host> observes it"
  local first_id owner_party registry_cid transaction emitter_cid emitter_argument summary validator_name observation
  local scan_summary core_state_cid
  first_id="$(read_state firstId)"
  owner_party="$(read_state migrationOwner | jq -r .partyId)"
  registry_cid="$(read_state migratedRegistry)"
  transaction="$(submit_interactive "registry-migration-register-$(date +%s)" "$(jq -n --arg p "$owner_party" '[$p]')" \
    "$(register_emitter_command "$EMITTER_REGISTRY_TEMPLATE" "$registry_cid" "$owner_party")" \
    "$(migration_owner_signers_json)" "[$(operator_disclosure "$REGISTRY_SUFFIX" "$registry_cid")]")"
  emitter_cid="$(created_contract_id "$transaction" "$EMITTER_SUFFIX")"
  emitter_argument="$(created_contract_argument "$transaction" "$EMITTER_SUFFIX")"
  [[ "$(jq -r .emitterId <<<"$emitter_argument")" == "$first_id" ]] || fail "the new emitter has id $(jq -r .emitterId <<<"$emitter_argument")"
  [[ "$(jq -r '.events[] | .ExercisedEvent? // empty | select(.choice == "RegisterEmitter") | .templateId | split(":")[0]' <<<"$transaction")" == "$CORE_V8_PACKAGE_ID" ]] \
    || fail "RegisterEmitter did not run under 0.8.0"
  echo "  PASS: RegisterEmitter under 0.8.0 created emitter id $first_id, address $(jq -r .emitterAddress <<<"$emitter_argument")"
  core_state_cid="$(core_contract_id coreState)"
  transaction="$(submit_interactive "registry-migration-publish-$(date +%s)" "$(jq -n --arg p "$owner_party" '[$p]')" \
    "$(exercise_command "$EMITTER_TEMPLATE" "$emitter_cid" PublishMessage \
      "$(jq -n --arg p "$owner_party" --arg cs "$core_state_cid" --arg payload "$(hex_of "registry migration first message")" \
        '{nonce: "1", payload: $payload, consistencyLevel: "0", coreStateCid: $cs, payer: $p, feeAllocation: null}')")" \
    "$(migration_owner_signers_json)" "[$(operator_disclosure ":Wormhole.Core.State:CoreState" "$core_state_cid")]")"
  summary="$(publish_summary "$transaction")"
  [[ "$(jq -r .emitterId <<<"$summary")" == "$first_id" && "$(jq -r .sequence <<<"$summary")" == "0" ]] \
    || fail "unexpected publish: $summary"
  for validator_name in "${GUARDIAN_HOSTS[@]}"; do
    observation="$(await_observation "v8-$validator_name" "$(jq -r .updateId <<<"$summary")" false)"
    [[ "$(jq -r .emitterAddress <<<"$observation")" == "$(jq -r .emitterAddress <<<"$emitter_argument")" ]] \
      || fail "v8-$validator_name derived $(jq -r .emitterAddress <<<"$observation"), the Emitter has $(jq -r .emitterAddress <<<"$emitter_argument")"
    echo "  PASS: v8-$validator_name observed: $(jq -c '{emitterChain, emitterAddress, sequence, updateId}' <<<"$observation")"
  done
  stop_watcher_on_each_host v8
  for validator_name in "${GUARDIAN_HOSTS[@]}"; do
    [[ "$(jq -c . "$WATCHER_DIR/v8-$validator_name.metrics.json")" == "{}" ]] || fail "v8-$validator_name dropped publishes: $(cat "$WATCHER_DIR/v8-$validator_name.metrics.json")"
  done
  scan_summary="$(run_scan_on "$SUBMITTING_VALIDATOR" 1)" || fail "the post-ceremony scan failed"
  [[ "$(jq -r .firstId <<<"$scan_summary")" == "$((first_id + 1))" ]] || fail "post-ceremony scan: $scan_summary"
  echo "  PASS: post-ceremony scan with one active registry: $(jq -c '{historyMaxEmitterId, activeRegistries, activeDuplicates, firstId}' <<<"$scan_summary")"
}

copy_harness_evidence() {
  local evidence_copy_dir="${EVIDENCE_LOG%.log}"
  mkdir -p "$evidence_copy_dir"
  cp "$WATCHER_DIR"/*.jsonl "$WATCHER_DIR"/*.log "$WATCHER_DIR"/*.metrics.json "$evidence_copy_dir"/ 2>/dev/null || true
  cp "$gg_work_dir"/*.err "$evidence_copy_dir"/ 2>/dev/null || true
  cp "$MIGRATION_STATE" "$evidence_copy_dir"/
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
  vet-v8) step_vet_v8 ;;
  cutover) step_cutover ;;
  switch-pin) step_switch_pin ;;
  unvet-v7) step_unvet_v7 ;;
  scan) step_scan ;;
  ceremony) step_ceremony ;;
  register) step_register ;;
  all)
    step_prepare
    step_vet_v8
    step_cutover
    step_switch_pin
    step_unvet_v7
    step_scan
    step_ceremony
    step_register
    ;;
  *)
    echo "usage: registry_migration_rehearsal.sh prepare|vet-v8|cutover|switch-pin|unvet-v7|scan|ceremony|register|all" >&2
    exit 2
    ;;
esac
echo "registry_migration_rehearsal.sh ${1}: PASS"
