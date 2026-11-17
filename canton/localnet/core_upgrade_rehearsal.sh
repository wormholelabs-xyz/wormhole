#!/usr/bin/env bash
# Rehearse the wormhole-core 0.6.0 to 0.7.0 upgrade (canton/README.md section 7.5) on LocalNet.
# Steps (run one, or "all" in order):
#   prepare       Vet 0.6.0 and unvet 0.7.0 on all three guardianGovernance hosts.
#   observe-v6    Start a watcher pinned to 0.6.0. Publish under 0.6.0. Expect an observation.
#   vet-v7        Upload the 0.7.0 DAR to all three hosts and vet it. The upload runs the upgrade check.
#   publish-v7    Publish under 0.7.0 on an Emitter that 0.6.0 created. Expect the 0.6.0 watcher to drop it.
#   reobserve-v7  Start a watcher pinned to 0.7.0. Reobserve the dropped message by update id.
#   ntt           Upload ntt 0.3.0, which calls 0.7.0. Run ntt_e2e.sh with a watcher pinned to 0.7.0.
#                 Expect each NTT PublishMessage to run under 0.7.0 on each host.
#   enforce-v7    A nonce above uint32: 0.7.0 rejects it, 0.6.0 commits it. Unvet 0.6.0 and ntt 0.2.0.
#   post-unvet    Expect 0.6.0 and ntt 0.2.0 submissions to fail, contracts that 0.6.0 created to work
#                 under 0.7.0, and ntt_e2e.sh to pass with ntt 0.3.0.
# Usage: core_upgrade_rehearsal.sh <step>|all
# Precondition: gg_publish_and_govern.sh ran; the CoreState message fee is 0;
# canton/core/.daml/dist/wormhole-core-0.7.0.dar and the ntt 0.3.0 DAR are built.
# The prepare step unvets 0.7.0. ntt 0.3.0 then fails until the vet-v7 step.
set -euo pipefail
# shellcheck source=gg_lib.sh
source "$(dirname "${BASH_SOURCE[0]}")/gg_lib.sh"

readonly CORE_V6_PACKAGE_ID="701a57c867e02d962f231bd9f0ff5186e16462a77289aa1b7aaba7fe95074c15"
readonly CORE_V7_PACKAGE_ID="7a22a1c4026b2a037b021063c4ff2bd5506be2e748aa9de8b7a6057c90c356eb"
# ntt 0.2.0 calls 0.6.0 by package id. ntt 0.3.0 calls 0.7.0.
readonly NTT_V2_PACKAGE_ID="8b9c3ee1c93b7039b39421b790c283f507c2b6a01171aa839873f220dc5ff84a"
readonly CORE_V6_DAR="$REPO_ROOT/canton/dars/wormhole-core-0.6.0.dar"
readonly CORE_V7_DAR="$REPO_ROOT/canton/core/.daml/dist/wormhole-core-0.7.0.dar"
readonly DPM_BIN_DIR="${DPM_BIN_DIR:-$HOME/.dpm/bin}"
readonly GUARDIAN_HOSTS=(app-provider sv app-user)
readonly UPGRADE_STATE="$LOCALNET_STATE_DIR/core-upgrade.json"
readonly WATCHER_DIR="$LOCALNET_STATE_DIR/core-upgrade-watcher"
readonly OBSERVATION_WAIT_SECONDS=60
# Time that the 0.6.0 watcher gets to show that it drops the 0.7.0 message.
readonly DROP_WINDOW_SECONDS=15
readonly NONCE_ABOVE_UINT32=4294967296
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

# vetted_core_versions <validator>: wormhole-core versions vetted by that validator's own participant, as JSON.
vetted_core_versions() {
  curl -sSf -X POST "$(json_api_url "$1")/v2/package-vetting/list" -H "@$ADMIN_AUTH_HEADER" \
    -H 'Content-Type: application/json' \
    -d "$(jq -n --arg pid "$(participant_id_of "$1")" \
      '{packageMetadataFilter: {packageNamePrefixes: ["wormhole-core"]}, topologyStateFilter: {participantIds: [$pid]}}')" \
    | jq -c '[.vettedPackages[]?.packages[] | select(.packageName == "wormhole-core") | "\(.packageVersion) \(.packageId[0:12])"] | unique'
}

# vetted_ntt_versions <validator>: ntt versions vetted by that validator's own participant, as JSON.
vetted_ntt_versions() {
  curl -sSf -X POST "$(json_api_url "$1")/v2/package-vetting/list" -H "@$ADMIN_AUTH_HEADER" \
    -H 'Content-Type: application/json' \
    -d "$(jq -n --arg pid "$(participant_id_of "$1")" \
      '{packageMetadataFilter: {packageNamePrefixes: ["ntt"]}, topologyStateFilter: {participantIds: [$pid]}}')" \
    | jq -c '[.vettedPackages[]?.packages[] | select(.packageName == "ntt") | "\(.packageVersion) \(.packageId[0:12])"] | unique'
}

await_core_vetting() {
  local validator_name="$1" expected_versions="$2" attempt
  for attempt in $(seq 1 60); do
    if [[ "$attempt" -gt 1 ]]; then sleep 1; fi
    if [[ "$(vetted_core_versions "$validator_name")" == "$expected_versions" ]]; then
      echo "  $validator_name vets wormhole-core $expected_versions"
      return 0
    fi
  done
  fail "$validator_name vets wormhole-core $(vetted_core_versions "$validator_name"), expected $expected_versions"
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

unvet_package() {
  local validator_name="$1" package_id="$2"
  curl -sSf -X POST "$(json_api_url "$validator_name")/v2/package-vetting/update" -H "@$ADMIN_AUTH_HEADER" \
    -H 'Content-Type: application/json' \
    -d "$(jq -n --arg packageId "$package_id" --arg sync "$LOGICAL_SYNCHRONIZER_ID" \
      '{changes: [{operation: {Unvet: {value: {packages: [{packageId: $packageId}]}}}}], dryRun: false, synchronizerId: $sync}')" \
    > /dev/null
  echo "  $validator_name: unvetted ${package_id:0:12}"
}

vet_package() {
  local validator_name="$1" package_id="$2"
  curl -sSf -X POST "$(json_api_url "$validator_name")/v2/package-vetting/update" -H "@$ADMIN_AUTH_HEADER" \
    -H 'Content-Type: application/json' \
    -d "$(jq -n --arg packageId "$package_id" --arg sync "$LOGICAL_SYNCHRONIZER_ID" \
      '{changes: [{operation: {Vet: {value: {packages: [{packageId: $packageId}]}}}}], dryRun: false, synchronizerId: $sync}')" \
    > /dev/null
  echo "  $validator_name: vetted ${package_id:0:12}"
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

operator_emitter_template_id() {
  active_contracts "$SUBMITTING_VALIDATOR" "$OPERATOR_PARTY" ":Wormhole.Core.State:Emitter" \
    | jq -er --arg op "$OPERATOR_PARTY" '[.[] | select(.createArgument.owner == $op)] | if length == 1 then .[0].templateId else error("expected one Operator Emitter") end'
}

# publish_summary <transaction-json>: update id, executing package of PublishMessage, sequence, nonce.
publish_summary() {
  jq -ec '[.events[] | .ExercisedEvent? // empty | select(.choice == "PublishMessage")]
    | if length != 1 then error("expected one PublishMessage event") else .[0] end
    | {executingPackageId: (.templateId | split(":")[0]), sequence: .exerciseResult._2.sequence,
       nonce: .exerciseResult._2.nonce, payload: .exerciseResult._2.payload}' <<<"$1" \
    | jq -c --arg updateId "$(jq -r .updateId <<<"$1")" '. + {updateId: $updateId}'
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

# await_observation <harness-name> <sequence> <payload-hex> <reobservation true|false>
await_observation() {
  local observations_file="$WATCHER_DIR/$1.jsonl" sequence="$2" payload_hex="$3" reobserved="$4" waited_seconds
  for ((waited_seconds = 0; waited_seconds < OBSERVATION_WAIT_SECONDS; waited_seconds++)); do
    if [[ -s "$observations_file" ]] && jq -se --argjson seq "$sequence" --arg payload "$payload_hex" --argjson reobserved "$reobserved" \
        '[.[] | select(.sequence == $seq and .payloadHex == $payload and .isReobservation == $reobserved)] | length == 1' \
        "$observations_file" > /dev/null; then
      echo "  watcher $1 observed: $(jq -c --argjson seq "$sequence" --arg payload "$payload_hex" \
        'select(.sequence == $seq and .payloadHex == $payload) | {emitterChain, sequence, nonce, updateId, isReobservation}' "$observations_file")"
      return 0
    fi
    sleep 1
  done
  fail "watcher $1 has no observation of sequence $sequence (reobservation $reobserved) after ${OBSERVATION_WAIT_SECONDS}s"
}

harness_is_running() {
  [[ -f "$WATCHER_DIR/$1.pid" ]] && kill -0 "$(cat "$WATCHER_DIR/$1.pid")" 2>/dev/null
}

# expect_rejection <output-prefix> <expected-text> <description>. The expected text holds letters, digits and spaces only.
expect_rejection() {
  grep -qF "$2" "$1.err" || { cat "$1.err" >&2; fail "$3: the error does not contain '$2'"; }
  echo "  PASS: $3 rejected ($(grep -oE '[A-Z][A-Z0-9_]{9,}' "$1.err" | head -n 1)): $(grep -oE "[^\"]{0,120}$2[^\"]{0,80}" "$1.err" | head -n 1)"
}

step_prepare() {
  echo "### prepare: 0.6.0 vetted, 0.7.0 not vetted, on each guardianGovernance host"
  check_dar_package_ids
  local validator_name
  for validator_name in "${GUARDIAN_HOSTS[@]}"; do
    upload_and_vet "$validator_name" "$CORE_V6_DAR"
    if [[ "$(vetted_core_versions "$validator_name")" == *"${CORE_V7_PACKAGE_ID:0:12}"* ]]; then
      unvet_package "$validator_name" "$CORE_V7_PACKAGE_ID"
    fi
  done
  for validator_name in "${GUARDIAN_HOSTS[@]}"; do
    await_core_vetting "$validator_name" "[\"0.6.0 ${CORE_V6_PACKAGE_ID:0:12}\"]"
  done
}

step_observe_v6() {
  echo "### 1. Watcher pinned to 0.6.0 observes a 0.6.0 PublishMessage"
  build_harness
  if harness_is_running v6; then stop_harness v6; fi
  run_harness v6 "$CORE_V6_PACKAGE_ID" "" 1800
  local payload_hex transaction summary
  echo "  input Emitter template: $(operator_emitter_template_id)"
  payload_hex="$(hex_of "core upgrade $(date +%s) under 0.6.0")"
  transaction="$(publish_on_operator_emitter "core-upgrade-v6-$(date +%s)" "$CORE_V6_PACKAGE_ID:Wormhole.Core.State:Emitter" 1 "$payload_hex")"
  summary="$(publish_summary "$transaction")"
  echo "  published: $summary"
  [[ "$(jq -r .executingPackageId <<<"$summary")" == "$CORE_V6_PACKAGE_ID" ]] || fail "PublishMessage did not execute under 0.6.0"
  await_observation v6 "$(jq -r .sequence <<<"$summary")" "$payload_hex" false
}

step_vet_v7() {
  echo "### 2. Upload and vet 0.7.0 on each guardianGovernance host"
  check_dar_package_ids
  local validator_name
  for validator_name in "${GUARDIAN_HOSTS[@]}"; do
    upload_and_vet "$validator_name" "$CORE_V7_DAR"
  done
  for validator_name in "${GUARDIAN_HOSTS[@]}"; do
    await_core_vetting "$validator_name" "[\"0.6.0 ${CORE_V6_PACKAGE_ID:0:12}\",\"0.7.0 ${CORE_V7_PACKAGE_ID:0:12}\"]"
  done
}

step_publish_v7() {
  echo "### 3. Publish under 0.7.0 on an Emitter that 0.6.0 created; the 0.6.0 watcher drops it"
  harness_is_running v6 || fail "the 0.6.0 watcher (v6) does not run; run observe-v6 first"
  local emitter_template payload_hex transaction summary update_id validator_name event_package
  emitter_template="$(operator_emitter_template_id)"
  echo "  input Emitter template: $emitter_template"
  [[ "$emitter_template" == "$CORE_V6_PACKAGE_ID:"* ]] || fail "the Operator Emitter was not created under 0.6.0"
  payload_hex="$(hex_of "core upgrade $(date +%s) under 0.7.0")"
  transaction="$(publish_on_operator_emitter "core-upgrade-v7-$(date +%s)" "$CORE_V7_PACKAGE_ID:Wormhole.Core.State:Emitter" 1 "$payload_hex")"
  summary="$(publish_summary "$transaction")"
  echo "  published: $summary"
  [[ "$(jq -r .executingPackageId <<<"$summary")" == "$CORE_V7_PACKAGE_ID" ]] || fail "PublishMessage did not execute under 0.7.0"
  update_id="$(jq -r .updateId <<<"$summary")"
  for validator_name in "${GUARDIAN_HOSTS[@]}"; do
    event_package="$(publish_package_on "$validator_name" "$update_id")"
    echo "  $validator_name: PublishMessage event package id $event_package"
    [[ "$event_package" == "$CORE_V7_PACKAGE_ID "* ]] || fail "$validator_name reports another package id"
  done
  record_state v7UpdateId "$update_id"
  record_state v7Sequence "$(jq -r .sequence <<<"$summary")"
  record_state v7PayloadHex "$payload_hex"
  sleep "$DROP_WINDOW_SECONDS"
  stop_harness v6
  if jq -se --arg payload "$payload_hex" '[.[] | select(.payloadHex == $payload)] | length > 0' "$WATCHER_DIR/v6.jsonl" > /dev/null; then
    fail "the 0.6.0 watcher observed the 0.7.0 message"
  fi
  echo "  PASS: the 0.6.0 watcher has no observation of sequence $(jq -r .sequence <<<"$summary") after ${DROP_WINDOW_SECONDS}s"
  echo "  v6 observations: $(jq -sc '[.[] | {sequence, nonce, updateId: .updateId[0:16]}]' "$WATCHER_DIR/v6.jsonl")"
}

step_reobserve_v7() {
  echo "### 4. Watcher pinned to 0.7.0 reobserves the dropped message by update id"
  local update_id payload_hex transaction summary
  update_id="$(read_state v7UpdateId)"
  [[ -x "$(harness_binary)" ]] || build_harness
  run_harness v7 "$CORE_V7_PACKAGE_ID" "$update_id" 900
  await_observation v7 "$(read_state v7Sequence)" "$(read_state v7PayloadHex)" true
  echo "  a publish by package name (no package id) while both versions are vetted:"
  payload_hex="$(hex_of "core upgrade $(date +%s) by package name")"
  transaction="$(publish_on_operator_emitter "core-upgrade-by-name-$(date +%s)" "$EMITTER_TEMPLATE" 1 "$payload_hex")"
  summary="$(publish_summary "$transaction")"
  echo "  published: $summary"
  [[ "$(jq -r .executingPackageId <<<"$summary")" == "$CORE_V7_PACKAGE_ID" ]] || fail "package-name selection did not pick 0.7.0"
  await_observation v7 "$(jq -r .sequence <<<"$summary")" "$payload_hex" false
  stop_harness v7
}

step_enforce_v7() {
  echo "### 5a. Nonce $NONCE_ABOVE_UINT32 (above uint32): rejected under 0.7.0, committed under 0.6.0"
  local payload_hex transaction summary validator_name
  payload_hex="$(hex_of "core upgrade nonce check")"
  if publish_on_operator_emitter "core-upgrade-nonce-v7-$(date +%s)" "$CORE_V7_PACKAGE_ID:Wormhole.Core.State:Emitter" \
      "$NONCE_ABOVE_UINT32" "$payload_hex" > "$gg_work_dir/nonce-v7.out" 2> "$gg_work_dir/nonce-v7.err"; then
    fail "0.7.0 committed a nonce above uint32"
  fi
  expect_rejection "$gg_work_dir/nonce-v7" "nonce exceeds uint32" "0.7.0 PublishMessage with nonce $NONCE_ABOVE_UINT32"
  transaction="$(publish_on_operator_emitter "core-upgrade-nonce-v6-$(date +%s)" "$CORE_V6_PACKAGE_ID:Wormhole.Core.State:Emitter" \
    "$NONCE_ABOVE_UINT32" "$payload_hex")"
  summary="$(publish_summary "$transaction")"
  [[ "$(jq -r .executingPackageId <<<"$summary")" == "$CORE_V6_PACKAGE_ID" ]] || fail "the nonce publish did not execute under 0.6.0"
  echo "  committed under 0.6.0: $summary"

  echo "### 5b. Unvet 0.6.0 and ntt 0.2.0 on each guardianGovernance host"
  for validator_name in "${GUARDIAN_HOSTS[@]}"; do
    [[ "$(vetted_ntt_versions "$validator_name")" == *'"0.3.0 '* ]] || fail "$validator_name does not vet ntt 0.3.0; run the ntt step first"
    unvet_package "$validator_name" "$CORE_V6_PACKAGE_ID"
    if [[ "$(vetted_ntt_versions "$validator_name")" == *"${NTT_V2_PACKAGE_ID:0:12}"* ]]; then
      unvet_package "$validator_name" "$NTT_V2_PACKAGE_ID"
    fi
  done
  for validator_name in "${GUARDIAN_HOSTS[@]}"; do
    await_core_vetting "$validator_name" "[\"0.7.0 ${CORE_V7_PACKAGE_ID:0:12}\"]"
    echo "  $validator_name vets ntt $(vetted_ntt_versions "$validator_name")"
  done
}

# create_deposit_preapproval <command-id> <template-id>: a DepositPreapproval of the Operator party. Prints the transaction JSON.
create_deposit_preapproval() {
  submit_interactive "$1" "$(jq -n --arg op "$OPERATOR_PARTY" '[$op]')" \
    "$(jq -n --arg templateId "$2" --arg op "$OPERATOR_PARTY" \
      '{CreateCommand: {templateId: $templateId, createArguments: {owner: $op, instrumentId: {admin: $op, id: "core-upgrade-rehearsal"}}}}')" \
    "$(operator_signers_json)"
}

step_post_unvet() {
  echo "### 7a. Each host vets only 0.7.0 of wormhole-core and does not vet ntt 0.2.0"
  local payload_hex transaction summary validator_name emitter_template verified_vaa
  for validator_name in "${GUARDIAN_HOSTS[@]}"; do
    await_core_vetting "$validator_name" "[\"0.7.0 ${CORE_V7_PACKAGE_ID:0:12}\"]"
    [[ "$(vetted_ntt_versions "$validator_name")" != *"${NTT_V2_PACKAGE_ID:0:12}"* ]] || fail "$validator_name vets ntt 0.2.0"
    echo "  $validator_name vets ntt $(vetted_ntt_versions "$validator_name")"
  done

  echo "### 7b. A submission that selects 0.6.0 is rejected"
  payload_hex="$(hex_of "core upgrade nonce check")"
  if publish_on_operator_emitter "core-upgrade-unvetted-v6-$(date +%s)" "$CORE_V6_PACKAGE_ID:Wormhole.Core.State:Emitter" \
      1 "$payload_hex" > "$gg_work_dir/unvetted-v6.out" 2> "$gg_work_dir/unvetted-v6.err"; then
    fail "a 0.6.0 PublishMessage committed after the unvet"
  fi
  expect_rejection "$gg_work_dir/unvetted-v6" "${CORE_V6_PACKAGE_ID:0:12}" "0.6.0 PublishMessage after the unvet"

  echo "### 7c. A submission that selects ntt 0.2.0 is rejected; the package name selects ntt 0.3.0"
  if create_deposit_preapproval "core-upgrade-unvetted-ntt-v2-$(date +%s)" "$NTT_V2_PACKAGE_ID:Wormhole.Ntt.Deposit:DepositPreapproval" \
      > "$gg_work_dir/unvetted-ntt-v2.out" 2> "$gg_work_dir/unvetted-ntt-v2.err"; then
    fail "an ntt 0.2.0 DepositPreapproval committed after the unvet"
  fi
  expect_rejection "$gg_work_dir/unvetted-ntt-v2" "${NTT_V2_PACKAGE_ID:0:12}" "ntt 0.2.0 DepositPreapproval after the unvet"
  transaction="$(create_deposit_preapproval "core-upgrade-ntt-by-name-$(date +%s)" "#ntt:Wormhole.Ntt.Deposit:DepositPreapproval")"
  echo "  PASS: DepositPreapproval by package name: $(jq -c '[.events[] | .CreatedEvent? // empty | {templateId: (.templateId | split(":")[0])}][0]' <<<"$transaction")"
  [[ "$(jq -r '[.events[] | .CreatedEvent? // empty | .templateId][0]' <<<"$transaction")" != "$NTT_V2_PACKAGE_ID:"* ]] \
    || fail "the package name selected ntt 0.2.0"

  echo "### 7d. Contracts that 0.6.0 created work under 0.7.0"
  emitter_template="$(operator_emitter_template_id)"
  echo "  input Emitter template: $emitter_template"
  [[ "$emitter_template" == "$CORE_V6_PACKAGE_ID:"* ]] || fail "the Operator Emitter was not created under 0.6.0"
  echo "  input CoreState template: $(active_contracts "$SUBMITTING_VALIDATOR" "$OPERATOR_PARTY" ":Wormhole.Core.State:CoreState" | jq -r '.[0].templateId')"
  payload_hex="$(hex_of "core upgrade $(date +%s) after the 0.6.0 unvet")"
  transaction="$(publish_on_operator_emitter "core-upgrade-after-unvet-$(date +%s)" "$EMITTER_TEMPLATE" 1 "$payload_hex")"
  summary="$(publish_summary "$transaction")"
  [[ "$(jq -r .executingPackageId <<<"$summary")" == "$CORE_V7_PACKAGE_ID" ]] || fail "the publish after the unvet did not execute under 0.7.0"
  echo "  PASS: PublishMessage by package name on the 0.6.0 Emitter and CoreState: $summary"
  verified_vaa="$(generate_vaa -kind message -signing-set-index "$CURRENT_GUARDIAN_SET_INDEX" \
    -signing-key-indices "$CURRENT_GUARDIAN_KEY_INDICES" -sequence "$(date +%s)")"
  transaction="$(verify_vaa_as_operator "core-upgrade-verify-$(date +%s)" "$(jq -r .vaaHex <<<"$verified_vaa")" "$(vaa_pub_keys_json "$verified_vaa")")"
  echo "  PASS: ParseAndVerifyVAA on the 0.6.0 CoreState: $(jq -c '[.events[] | .ExercisedEvent? // empty | select(.choice == "ParseAndVerifyVAA")
    | {templateId: (.templateId | split(":")[0]), guardianSetIndex: .exerciseResult.guardianSetIndex}][0]' <<<"$transaction")"

  echo "### 7e. ntt_e2e.sh with ntt 0.3.0 and a watcher pinned to 0.7.0"
  run_ntt_e2e_under_v7 true
}

# run_ntt_e2e_under_v7 <upload-dars: true|false>: ntt_e2e.sh with the watcher pinned to 0.7.0, then the package
# that ran each NTT PublishMessage, as each host sees it.
run_ntt_e2e_under_v7() {
  local deployment_key update_id validator_name event_package
  NTT_UPLOAD_DARS="$1" NTT_CORE_PACKAGE_ID="$CORE_V7_PACKAGE_ID" NTT_GUARDIAN_SET_INDEX="$CURRENT_GUARDIAN_SET_INDEX" \
    NTT_GUARDIAN_KEY_INDICES="$CURRENT_GUARDIAN_KEY_INDICES" "$LOCALNET_SCRIPT_DIR/ntt_e2e.sh"
  for deployment_key in burnMint.burnUpdateId lockUnlock.lockUpdateId; do
    update_id="$(jq -er ".$deployment_key" "$LOCALNET_STATE_DIR/ntt-contracts.json")"
    for validator_name in "${GUARDIAN_HOSTS[@]}"; do
      event_package="$(publish_package_on "$validator_name" "$update_id")"
      echo "  $validator_name: NTT $deployment_key $update_id: PublishMessage event package id $event_package"
      [[ "$event_package" == "$CORE_V7_PACKAGE_ID "* ]] || fail "the NTT PublishMessage did not execute under 0.7.0 on $validator_name"
    done
  done
  echo "  PASS: each NTT outbound PublishMessage ran under ${CORE_V7_PACKAGE_ID:0:12} on each host"
}

step_ntt() {
  echo "### 6a. Upload ntt 0.3.0, which calls 0.7.0"
  local validator_name
  "$LOCALNET_SCRIPT_DIR/ntt_upload_dars.sh"
  for validator_name in "${GUARDIAN_HOSTS[@]}"; do
    echo "  $validator_name vets wormhole-core $(vetted_core_versions "$validator_name"), ntt $(vetted_ntt_versions "$validator_name")"
  done
  echo "### 6b. ntt_e2e.sh with a watcher pinned to 0.7.0"
  run_ntt_e2e_under_v7 false
}

copy_harness_evidence() {
  local evidence_copy_dir="${EVIDENCE_LOG%.log}"
  mkdir -p "$evidence_copy_dir"
  cp "$WATCHER_DIR"/*.jsonl "$WATCHER_DIR"/*.log "$evidence_copy_dir"/ 2>/dev/null || true
  cp "$UPGRADE_STATE" "$evidence_copy_dir"/
}
trap 'copy_harness_evidence; rm -rf "$gg_work_dir"' EXIT

case "${1:-}" in
  prepare) step_prepare ;;
  observe-v6) step_observe_v6 ;;
  vet-v7) step_vet_v7 ;;
  publish-v7) step_publish_v7 ;;
  reobserve-v7) step_reobserve_v7 ;;
  enforce-v7) step_enforce_v7 ;;
  ntt) step_ntt ;;
  post-unvet) step_post_unvet ;;
  all)
    step_prepare
    step_observe_v6
    step_vet_v7
    step_publish_v7
    step_reobserve_v7
    step_ntt
    step_enforce_v7
    step_post_unvet
    ;;
  *)
    echo "usage: core_upgrade_rehearsal.sh prepare|observe-v6|vet-v7|publish-v7|reobserve-v7|ntt|enforce-v7|post-unvet|all" >&2
    exit 2
    ;;
esac
echo "core_upgrade_rehearsal.sh ${1}: PASS"
