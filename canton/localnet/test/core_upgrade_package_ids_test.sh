#!/usr/bin/env bash
# Table-driven test of core_upgrade_package_ids.sh: the rehearsal DARs have the pinned package ids.
# Needs bash, jq and unzip. The ntt 0.2.0 case runs when NTT_REPO_DIR (default ../ntt) has the vendored DAR.
# Usage: canton/localnet/test/core_upgrade_package_ids_test.sh
set -uo pipefail
LOCALNET_TEST_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
readonly LOCALNET_TEST_DIR
# shellcheck source=SCRIPTDIR/../env.sh
source "$LOCALNET_TEST_DIR/../env.sh"
# shellcheck source=SCRIPTDIR/../ntt_offline_lib.sh
source "$LOCALNET_TEST_DIR/../ntt_offline_lib.sh"
# shellcheck source=SCRIPTDIR/../core_upgrade_package_ids.sh
source "$LOCALNET_TEST_DIR/../core_upgrade_package_ids.sh"

failure_count=0
work_dir="$(mktemp -d)"
trap 'rm -rf "$work_dir"' EXIT

# check_case <case-name> <expected-status> <stderr-fragment or "-"> <command...>
check_case() {
  local case_name="$1" expected_status="$2" stderr_fragment="$3" actual_status=0
  shift 3
  "$@" > /dev/null 2> "$work_dir/stderr" || actual_status=$?
  if [[ "$actual_status" != "$expected_status" ]]; then
    echo "FAIL: $case_name: status $actual_status, expected $expected_status ($(head -c 300 "$work_dir/stderr"))" >&2
    failure_count=$((failure_count + 1))
    return
  fi
  if [[ "$stderr_fragment" != "-" ]] && ! grep -qF -- "$stderr_fragment" "$work_dir/stderr"; then
    echo "FAIL: $case_name: stderr '$(head -c 300 "$work_dir/stderr")' lacks '$stderr_fragment'" >&2
    failure_count=$((failure_count + 1))
    return
  fi
  echo "ok: $case_name"
}

check_case "core 0.6.0 DAR is the pinned 0.6.0 package" 0 - \
  check_rehearsal_dar "$CORE_V6_DAR" "wormhole-core 0.6.0" "$CORE_V6_PACKAGE_ID"
check_case "core 0.7.0 DAR is the vendored 0.7.0 package" 0 - \
  check_rehearsal_dar "$CORE_V7_DAR" "wormhole-core 0.7.0" "$CORE_V7_PACKAGE_ID"
check_case "core 0.7.0 DAR is under canton/dars" 0 - \
  test "$CORE_V7_DAR" == "$REPO_ROOT/canton/dars/wormhole-core-0.7.0.dar"
check_case "core 0.6.0 DAR with the 0.7.0 id fails" 1 "expected $CORE_V7_PACKAGE_ID" \
  check_rehearsal_dar "$CORE_V6_DAR" "wormhole-core 0.6.0" "$CORE_V7_PACKAGE_ID"
check_case "core 0.7.0 DAR with version 0.6.0 fails" 1 "is wormhole-core 0.7.0, expected wormhole-core 0.6.0" \
  check_rehearsal_dar "$CORE_V7_DAR" "wormhole-core 0.6.0" "$CORE_V7_PACKAGE_ID"
check_case "missing DAR fails" 1 "DAR not found" \
  check_rehearsal_dar "$work_dir/absent.dar" "wormhole-core 0.6.0" "$CORE_V6_PACKAGE_ID"
check_case "the 0.6.0 and 0.7.0 ids differ" 1 - \
  test "$CORE_V6_PACKAGE_ID" == "$CORE_V7_PACKAGE_ID"
check_case "the ntt 0.2.0 and 0.3.0 ids differ" 1 - \
  test "$NTT_V2_PACKAGE_ID" == "$NTT_V3_PACKAGE_ID"
check_case "core 0.6.0 DAR has CORE_V6_VERSION" 0 - \
  check_rehearsal_dar "$CORE_V6_DAR" "wormhole-core $CORE_V6_VERSION" "$CORE_V6_PACKAGE_ID"
check_case "core 0.7.0 DAR has CORE_V7_VERSION" 0 - \
  check_rehearsal_dar "$CORE_V7_DAR" "wormhole-core $CORE_V7_VERSION" "$CORE_V7_PACKAGE_ID"

# check_approved_list_is_core_v6_and_v7 <file>
check_approved_list_is_core_v6_and_v7() {
  local expected_approved_list
  expected_approved_list="$(jq -nc --arg v6 "$CORE_V6_PACKAGE_ID" --arg v7 "$CORE_V7_PACKAGE_ID" \
    '[{packageId: $v6, version: "0.6.0"}, {packageId: $v7, version: "0.7.0"}]')"
  write_rehearsal_approved_core_packages "$1" && [[ "$(jq -c . "$1")" == "$expected_approved_list" ]]
}
check_case "the rehearsal approved list has exactly core 0.6.0 and 0.7.0" 0 - \
  check_approved_list_is_core_v6_and_v7 "$work_dir/approved.json"
check_case "the rehearsal approved list rejects a missing directory" 1 - \
  write_rehearsal_approved_core_packages "$work_dir/absent/approved.json"

if [[ -f "$NTT_V2_DAR" ]]; then
  check_case "ntt 0.2.0 DAR is the pinned 0.2.0 package" 0 - \
    check_rehearsal_dar "$NTT_V2_DAR" "ntt 0.2.0" "$NTT_V2_PACKAGE_ID"
else
  echo "skip: ntt 0.2.0 DAR not found at $NTT_V2_DAR"
fi

if [[ "$failure_count" -ne 0 ]]; then
  echo "core_upgrade_package_ids_test.sh: $failure_count failures" >&2
  exit 1
fi
echo "core_upgrade_package_ids_test.sh: PASS"
