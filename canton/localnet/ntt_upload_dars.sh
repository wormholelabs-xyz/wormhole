#!/usr/bin/env bash
# Upload the pinned ntt and wormhole-genesis DARs to all three LocalNet participants and vet their unvetted packages.
# Stops if a DAR differs from the pins in ntt_lib.sh, or if a participant vets a wormhole-core other than the pin
# as its newest core. Do not use this script to vet wormhole-core; upload_dars.sh vets it.
# Usage: ntt_upload_dars.sh
# Precondition: gg_host.sh ran; NTT_REPO_DIR (default ../ntt) has the built ntt and wormhole-genesis DARs.
set -euo pipefail
# shellcheck source=ntt_lib.sh
source "$(dirname "${BASH_SOURCE[0]}")/ntt_lib.sh"

localnet_core_dar="$(localnet_core_dar_path)"
[[ -f "$localnet_core_dar" ]] || fail "DAR not found: $localnet_core_dar (build it with 'dpm build --all' in canton)"
[[ "$(dar_main_package_name_and_version "$localnet_core_dar")" == "wormhole-core $WORMHOLE_CORE_VERSION" ]] \
  || fail "$localnet_core_dar is not wormhole-core $WORMHOLE_CORE_VERSION"
[[ "$(dar_main_package_id "$localnet_core_dar")" == "$WORMHOLE_CORE_PACKAGE_ID" ]] \
  || fail "$localnet_core_dar is wormhole-core $(dar_main_package_id "$localnet_core_dar"), expected $WORMHOLE_CORE_PACKAGE_ID"

[[ -f "$NTT_DAR" ]] || fail "DAR not found: $NTT_DAR (build it with 'dpm build --all' in $NTT_CANTON_DIR)"
[[ "$(dar_main_package_name_and_version "$NTT_DAR")" == "ntt $NTT_VERSION" ]] \
  || fail "the main package of $NTT_DAR is not ntt $NTT_VERSION"
[[ "$(dar_main_package_id "$NTT_DAR")" == "$NTT_PACKAGE_ID" ]] \
  || fail "$NTT_DAR is ntt $(dar_main_package_id "$NTT_DAR"), expected $NTT_PACKAGE_ID"
check_core_package_id_of_dar "$NTT_DAR" "$WORMHOLE_CORE_PACKAGE_ID" || fail "$NTT_DAR does not call the pinned wormhole-core"
[[ "$(dar_dependency_package_ids "$NTT_DAR" token-cip0056)" == "$TOKEN_CIP0056_PACKAGE_ID" ]] \
  || fail "$NTT_DAR does not carry exactly token-cip0056 $TOKEN_CIP0056_PACKAGE_ID"
[[ -f "$WORMHOLE_GENESIS_DAR" ]] || fail "DAR not found: $WORMHOLE_GENESIS_DAR (build it with 'dpm build --all' in $NTT_CANTON_DIR)"
[[ "$(dar_main_package_name_and_version "$WORMHOLE_GENESIS_DAR")" == "wormhole-genesis $WORMHOLE_GENESIS_VERSION" ]] \
  || fail "the main package of $WORMHOLE_GENESIS_DAR is not wormhole-genesis $WORMHOLE_GENESIS_VERSION"
[[ "$(dar_main_package_id "$WORMHOLE_GENESIS_DAR")" == "$WORMHOLE_GENESIS_PACKAGE_ID" ]] \
  || fail "$WORMHOLE_GENESIS_DAR is wormhole-genesis $(dar_main_package_id "$WORMHOLE_GENESIS_DAR"), expected $WORMHOLE_GENESIS_PACKAGE_ID"
check_core_package_id_of_dar "$WORMHOLE_GENESIS_DAR" "$WORMHOLE_CORE_PACKAGE_ID" \
  || fail "$WORMHOLE_GENESIS_DAR does not call the pinned wormhole-core"
[[ "$(dar_dependency_package_ids "$WORMHOLE_GENESIS_DAR" ntt)" == "$NTT_PACKAGE_ID" ]] \
  || fail "$WORMHOLE_GENESIS_DAR does not carry exactly ntt $NTT_PACKAGE_ID"
[[ "$(dar_dependency_package_ids "$WORMHOLE_GENESIS_DAR" token-cip0056)" == "$TOKEN_CIP0056_PACKAGE_ID" ]] \
  || fail "$WORMHOLE_GENESIS_DAR does not carry exactly token-cip0056 $TOKEN_CIP0056_PACKAGE_ID"
readonly UPLOADED_DARS=("$NTT_DAR" "$WORMHOLE_GENESIS_DAR")
dar_package_ids_json="$(for dar in "${UPLOADED_DARS[@]}"; do dar_package_ids "$dar"; done | sort -u | jq -Rsc 'split("\n") | map(select(length > 0))')"
echo "NTT DARs: ntt $NTT_VERSION $NTT_PACKAGE_ID, wormhole-genesis $WORMHOLE_GENESIS_VERSION $WORMHOLE_GENESIS_PACKAGE_ID, wormhole-core $WORMHOLE_CORE_VERSION ${WORMHOLE_CORE_PACKAGE_ID:0:12}, $(jq length <<<"$dar_package_ids_json") packages"

# vetted_packages <validator>: packages that the participant of <validator> vets, as [{packageId, name, version}].
vetted_packages() {
  local ledger_json_url participant_id
  ledger_json_url="$(json_api_url "$1")"
  participant_id="$(curl --max-time 120 -sSf "$ledger_json_url/v2/parties/participant-id" -H "@$ADMIN_AUTH_HEADER" | jq -er .participantId)"
  curl --max-time 120 -sSf -X POST "$ledger_json_url/v2/package-vetting/list" -H "@$ADMIN_AUTH_HEADER" -H 'Content-Type: application/json' \
    -d "$(jq -n --arg pid "$participant_id" '{topologyStateFilter: {participantIds: [$pid]}}')" \
    | jq -ec '[.vettedPackages[]?.packages[] | {packageId, name: .packageName, version: .packageVersion}] | unique'
}

# unvetted_dar_package_ids <validator>: ids of the unvetted packages of the uploaded DARs on the participant, as a JSON array.
unvetted_dar_package_ids() {
  jq -nc --argjson dar "$dar_package_ids_json" --argjson vetted "$(vetted_packages "$1")" \
    '($vetted | map(.packageId)) as $vetted_ids | [$dar[] | select(. as $id | $vetted_ids | index($id) | not)]'
}

# check_core_vetting <validator>: the participant vets the pinned core and no newer core version.
check_core_vetting() {
  local validator_name="$1" vetted newest_vetted_core_version
  vetted="$(vetted_packages "$validator_name")"
  jq -e --arg id "$WORMHOLE_CORE_PACKAGE_ID" 'any(.[]; .packageId == $id)' <<<"$vetted" > /dev/null \
    || fail "$validator_name does not vet wormhole-core ${WORMHOLE_CORE_PACKAGE_ID:0:12}; run upload_dars.sh first"
  newest_vetted_core_version="$(jq -r '.[] | select(.name == "wormhole-core") | .version' <<<"$vetted" | sort -V | tail -n 1)"
  [[ "$newest_vetted_core_version" == "$WORMHOLE_CORE_VERSION" ]] \
    || fail "$validator_name vets wormhole-core $newest_vetted_core_version; the NTT DAR calls $WORMHOLE_CORE_VERSION"
  echo "$validator_name vets wormhole-core $(jq -c '[.[] | select(.name == "wormhole-core") | "\(.version) \(.packageId[0:12])"]' <<<"$vetted")"
}

# upload_without_vetting <validator> <dar>
upload_without_vetting() {
  local validator_name="$1" dar="$2" http_status
  http_status="$(curl --max-time 120 -sS -o "$gg_work_dir/upload.out" -w '%{http_code}' \
    -X POST "$(json_api_url "$validator_name")/v2/dars?vetAllPackages=false" \
    -H "@$ADMIN_AUTH_HEADER" -H 'Content-Type: application/octet-stream' --data-binary "@$dar")"
  [[ "$http_status" == "200" ]] || fail "upload of $(basename "$dar") to $validator_name: HTTP $http_status $(cat "$gg_work_dir/upload.out")"
}

# vet_packages <validator> <package-ids-json>
vet_packages() {
  curl --max-time 120 -sSf -X POST "$(json_api_url "$1")/v2/package-vetting/update" -H "@$ADMIN_AUTH_HEADER" \
    -H 'Content-Type: application/json' \
    -d "$(jq -n --argjson ids "$2" --arg sync "$LOGICAL_SYNCHRONIZER_ID" \
      '{changes: [{operation: {Vet: {value: {packages: [$ids[] | {packageId: .}]}}}}], dryRun: false, synchronizerId: $sync}')" \
    > /dev/null
}

# await_all_dar_packages_vetted <validator>: topology changes take effect asynchronously.
await_all_dar_packages_vetted() {
  local validator_name="$1" attempt
  for attempt in $(seq 1 60); do
    if [[ "$attempt" -gt 1 ]]; then sleep 1; fi
    if [[ "$(unvetted_dar_package_ids "$validator_name" | jq length)" -eq 0 ]]; then
      return 0
    fi
  done
  fail "$validator_name does not vet each package of the uploaded DARs after 60s"
}

for validator_name in "${GUARDIAN_HOSTS[@]}"; do
  check_core_vetting "$validator_name"
done
for validator_name in "${GUARDIAN_HOSTS[@]}"; do
  for dar in "${UPLOADED_DARS[@]}"; do
    upload_without_vetting "$validator_name" "$dar"
  done
  missing_package_ids="$(unvetted_dar_package_ids "$validator_name")"
  if jq -e --arg core "$WORMHOLE_CORE_PACKAGE_ID" 'index($core) != null' <<<"$missing_package_ids" > /dev/null; then
    fail "$validator_name: the uploaded DARs need an unvetted wormhole-core package"
  fi
  if [[ "$(jq length <<<"$missing_package_ids")" -gt 0 ]]; then
    vet_packages "$validator_name" "$missing_package_ids"
  fi
  await_all_dar_packages_vetted "$validator_name"
  check_core_vetting "$validator_name"
  echo "$validator_name: uploaded $(basename "$NTT_DAR") and $(basename "$WORMHOLE_GENESIS_DAR"); vetted $(jq -c 'map(.[0:12])' <<<"$missing_package_ids")"
done
echo "ntt_upload_dars.sh: PASS"
