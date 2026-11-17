#!/usr/bin/env bash
# A7 rehearsal: a transaction with guardianGovernance as informee runs only on packages that every
# guardianGovernance host vetted, not only a confirmation-threshold number of hosts.
#   1. Vet wormhole-localnet-vetting-probe 1.0.0 on all three hosts.
#   2. Vet 2.0.0 on app-provider only (unvet it on sv and app-user if a previous run vetted it).
#   3. Create VettingProbe with the 2.0.0 package id. Expect a rejection that names sv and app-user.
#   4. Create VettingProbe by package name. Expect commit on 1.0.0 (topology-aware package selection).
#   5. Vet 2.0.0 on sv (2 of 3 hosts, equal to the confirmation threshold). Repeat step 3.
#      Expect a rejection that names app-user only.
#   6. Vet 2.0.0 on app-user. Repeat step 3. Expect commit on 2.0.0.
# Usage: gg_package_vetting.sh
# Precondition: gg_host.sh ran.
set -euo pipefail
# shellcheck source=gg_lib.sh
source "$(dirname "${BASH_SOURCE[0]}")/gg_lib.sh"

readonly PROBE_PACKAGE_NAME="wormhole-localnet-vetting-probe"
readonly PROBE_BUILD_DIR="$LOCALNET_STATE_DIR/probe-build"
readonly DPM_BIN_DIR="${DPM_BIN_DIR:-$HOME/.dpm/bin}"

# The canton/ multi-package.yaml holds one version per package name, so each version builds in a copy.
probe_dar() {
  local major_version="$1"
  local dar_path="$PROBE_BUILD_DIR/v$major_version/.daml/dist/$PROBE_PACKAGE_NAME-$major_version.0.0.dar"
  if [[ ! -f "$dar_path" ]]; then
    rm -rf "$PROBE_BUILD_DIR/v$major_version"
    mkdir -p "$PROBE_BUILD_DIR"
    cp -R "$LOCALNET_SCRIPT_DIR/vetting_probe/v$major_version" "$PROBE_BUILD_DIR/v$major_version"
    (cd "$PROBE_BUILD_DIR/v$major_version" && PATH="$DPM_BIN_DIR:$PATH" dpm build >&2)
  fi
  echo "$dar_path"
}

upload_and_vet() {
  local validator_name="$1" dar_path="$2" http_status
  http_status="$(curl -sS -o "$gg_work_dir/upload.out" -w '%{http_code}' -X POST \
    "$(json_api_url "$validator_name")/v2/dars?vetAllPackages=true" \
    -H "@$ADMIN_AUTH_HEADER" -H 'Content-Type: application/octet-stream' --data-binary "@$dar_path")"
  if [[ "$http_status" != "200" ]]; then
    echo "Upload of $dar_path to $validator_name failed with HTTP $http_status: $(cat "$gg_work_dir/upload.out")" >&2
    exit 1
  fi
}

participant_id_of() {
  curl -sSf "$(json_api_url "$1")/v2/parties/participant-id" -H "@$ADMIN_AUTH_HEADER" | jq -er .participantId
}

# vetted_probe_versions <validator>: probe versions vetted by that validator's own participant.
vetted_probe_versions() {
  local participant_id
  participant_id="$(participant_id_of "$1")"
  curl -sSf -X POST "$(json_api_url "$1")/v2/package-vetting/list" -H "@$ADMIN_AUTH_HEADER" \
    -H 'Content-Type: application/json' \
    -d "$(jq -n --arg prefix "$PROBE_PACKAGE_NAME" --arg pid "$participant_id" \
      '{packageMetadataFilter: {packageNamePrefixes: [$prefix]}, topologyStateFilter: {participantIds: [$pid]}}')" \
    | jq -c '[.vettedPackages[]?.packages[] | .packageVersion] | sort'
}

unvet_probe_version() {
  local validator_name="$1" package_version="$2"
  curl -sSf -X POST "$(json_api_url "$validator_name")/v2/package-vetting/update" -H "@$ADMIN_AUTH_HEADER" \
    -H 'Content-Type: application/json' \
    -d "$(jq -n --arg name "$PROBE_PACKAGE_NAME" --arg version "$package_version" --arg sync "$LOGICAL_SYNCHRONIZER_ID" \
      '{changes: [{operation: {Unvet: {value: {packages: [{packageName: $name, packageVersion: $version}]}}}}], dryRun: false, synchronizerId: $sync}')" \
    > /dev/null
}

# await_vetting_state <validator> <expected-versions-json>: bounded wait for the topology change to apply.
await_vetting_state() {
  local attempt
  for attempt in $(seq 1 60); do
    if [[ "$attempt" -gt 1 ]]; then sleep 1; fi
    if [[ "$(vetted_probe_versions "$1")" == "$2" ]]; then
      return 0
    fi
  done
  echo "Vetting state of $1 is $(vetted_probe_versions "$1"), expected $2" >&2
  exit 1
}

create_vetting_probe() {
  local command_id="$1" template_id="$2" create_arguments="$3"
  submit_interactive "$command_id" "$(jq -n --arg op "$OPERATOR_PARTY" '[$op]')" \
    "$(jq -n --arg templateId "$template_id" --argjson createArguments "$create_arguments" \
      '{CreateCommand: {templateId: $templateId, createArguments: $createArguments}}')" \
    "$(operator_signers_json)"
}

created_package_version_label() {
  jq -r '[.events[] | .CreatedEvent? // empty | .templateId][0]' <<<"$1"
}

echo "Step 1: vet $PROBE_PACKAGE_NAME 1.0.0 on all hosts"
v1_dar="$(probe_dar 1)"
for validator_name in app-provider sv app-user; do
  upload_and_vet "$validator_name" "$v1_dar"
done

echo "Step 2: vet 2.0.0 on app-provider only"
v2_dar="$(probe_dar 2)"
upload_and_vet app-provider "$v2_dar"
for validator_name in sv app-user; do
  if [[ "$(vetted_probe_versions "$validator_name")" == *2.0.0* ]]; then
    unvet_probe_version "$validator_name" 2.0.0
  fi
done
await_vetting_state app-provider '["1.0.0","2.0.0"]'
await_vetting_state sv '["1.0.0"]'
await_vetting_state app-user '["1.0.0"]'
for validator_name in app-provider sv app-user; do
  echo "  $validator_name vets $(vetted_probe_versions "$validator_name")"
done
v2_package_id="$(curl -sSf -X POST "$(json_api_url app-provider)/v2/package-vetting/list" -H "@$ADMIN_AUTH_HEADER" \
  -H 'Content-Type: application/json' -d "$(jq -n --arg prefix "$PROBE_PACKAGE_NAME" '{packageMetadataFilter: {packageNamePrefixes: [$prefix]}}')" \
  | jq -er '[.vettedPackages[].packages[] | select(.packageVersion == "2.0.0") | .packageId] | unique | if length == 1 then .[0] else error("expected one 2.0.0 package id") end')"
echo "  2.0.0 package id $v2_package_id"

v2_arguments="$(jq -n --arg op "$OPERATOR_PARTY" --arg gg "$GG_PARTY" '{operator: $op, guardianGovernance: $gg, label: "a7", revision: "2"}')"
v1_arguments="$(jq -n --arg op "$OPERATOR_PARTY" --arg gg "$GG_PARTY" '{operator: $op, guardianGovernance: $gg, label: "a7"}')"

# Canton abbreviates ids in the error text: "Participant PAR::<name>::<first 12 fingerprint chars>... has not vetted <first 12 package id chars>...".
not_vetted_error_line() {
  local participant_id="$1" participant_prefix participant_fingerprint
  participant_prefix="${participant_id%::*}"
  participant_fingerprint="${participant_id##*::}"
  echo "Participant $participant_prefix::${participant_fingerprint:0:12}... has not vetted ${v2_package_id:0:12}"
}

# expect_not_vetted_rejection <output-prefix> <validators that vet 2.0.0> <validators that do not>
expect_not_vetted_rejection() {
  local output_prefix="$1" vetting_validators="$2" missing_validators="$3" validator_name error_line
  if ! grep -qF 'INVALID_PRESCRIBED_SYNCHRONIZER_ID' "$output_prefix.err" \
      || ! grep -qF 'has not vetted' "$output_prefix.err"; then
    echo "FAIL: expected INVALID_PRESCRIBED_SYNCHRONIZER_ID with 'has not vetted'" >&2
    cat "$output_prefix.err" >&2
    exit 1
  fi
  for validator_name in $missing_validators; do
    error_line="$(not_vetted_error_line "$(participant_id_of "$validator_name")")"
    if ! grep -qF "$error_line" "$output_prefix.err"; then
      echo "FAIL: the error does not contain: $error_line" >&2
      cat "$output_prefix.err" >&2
      exit 1
    fi
  done
  for validator_name in $vetting_validators; do
    error_line="$(not_vetted_error_line "$(participant_id_of "$validator_name")")"
    if grep -qF "$error_line" "$output_prefix.err"; then
      echo "FAIL: the error names $validator_name, which vets 2.0.0: $error_line" >&2
      exit 1
    fi
  done
  echo "PASS: rejected; not vetted on: $missing_validators"
}

for validator_name in app-provider sv app-user; do
  echo "  $validator_name is $(participant_id_of "$validator_name")"
done

echo "Step 3: create VettingProbe on 2.0.0 by package id (vetted on app-provider only)"
if create_vetting_probe "vetting-v2-one-host-$(date +%s)" "$v2_package_id:VettingProbe:VettingProbe" "$v2_arguments" \
    > "$gg_work_dir/3.out" 2> "$gg_work_dir/3.err"; then
  echo "FAIL: 2.0.0 committed while only app-provider vets it" >&2
  exit 1
fi
expect_not_vetted_rejection "$gg_work_dir/3" "app-provider" "sv app-user"

echo "Step 4: create VettingProbe by package name"
by_name_transaction="$(create_vetting_probe "vetting-by-name-$(date +%s)" "#$PROBE_PACKAGE_NAME:VettingProbe:VettingProbe" "$v1_arguments")"
selected_template="$(created_package_version_label "$by_name_transaction")"
if [[ "$selected_template" == "$v2_package_id:"* ]]; then
  echo "FAIL: package-name selection picked 2.0.0" >&2
  exit 1
fi
echo "PASS: committed on $selected_template (not 2.0.0)"

echo "Step 5: vet 2.0.0 on sv (2 of 3 hosts), then repeat step 3"
upload_and_vet sv "$v2_dar"
await_vetting_state sv '["1.0.0","2.0.0"]'
await_vetting_state app-user '["1.0.0"]'
if create_vetting_probe "vetting-v2-two-hosts-$(date +%s)" "$v2_package_id:VettingProbe:VettingProbe" "$v2_arguments" \
    > "$gg_work_dir/5.out" 2> "$gg_work_dir/5.err"; then
  echo "FAIL: 2.0.0 committed while app-user does not vet it" >&2
  exit 1
fi
expect_not_vetted_rejection "$gg_work_dir/5" "app-provider sv" "app-user"

echo "Step 6: vet 2.0.0 on app-user, then repeat step 3"
upload_and_vet app-user "$v2_dar"
await_vetting_state app-user '["1.0.0","2.0.0"]'
v2_transaction="$(create_vetting_probe "vetting-v2-all-hosts-$(date +%s)" "$v2_package_id:VettingProbe:VettingProbe" "$v2_arguments")"
selected_template="$(created_package_version_label "$v2_transaction")"
if [[ "$selected_template" != "$v2_package_id:"* ]]; then
  echo "FAIL: expected a create on 2.0.0, got $selected_template" >&2
  exit 1
fi
echo "PASS: committed on $selected_template"
