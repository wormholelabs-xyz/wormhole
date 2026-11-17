#!/usr/bin/env bash
# Table-driven test of ntt_offline_lib.sh. Needs bash, jq and unzip.
# Usage: canton/localnet/test/ntt_lib_test.sh
set -uo pipefail
LOCALNET_TEST_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
readonly LOCALNET_TEST_DIR
# shellcheck source=SCRIPTDIR/../ntt_offline_lib.sh
source "$LOCALNET_TEST_DIR/../ntt_offline_lib.sh"

failure_count=0
work_dir="$(mktemp -d)"
trap 'rm -rf "$work_dir"' EXIT

# check_case <case-name> <expected-status> <expected-stdout or "-"> <stderr-fragment or "-"> <command...>
check_case() {
  local case_name="$1" expected_status="$2" expected_stdout="$3" stderr_fragment="$4" actual_status=0 actual_stdout
  shift 4
  actual_stdout="$("$@" 2> "$work_dir/stderr")" || actual_status=$?
  if [[ "$actual_status" != "$expected_status" ]]; then
    echo "FAIL: $case_name: status $actual_status, expected $expected_status ($(head -c 300 "$work_dir/stderr"))" >&2
    failure_count=$((failure_count + 1))
    return
  fi
  if [[ "$expected_stdout" != "-" && "$actual_stdout" != "$expected_stdout" ]]; then
    echo "FAIL: $case_name: output '$actual_stdout', expected '$expected_stdout'" >&2
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

# check_text_has_line <text> <line>
check_text_has_line() {
  grep -qxF -- "$2" <<<"$1"
}

readonly CREATED_COIN_TRANSACTION='{"events": [
  {"CreatedEvent": {"templateId": "pkg:Token.CIP0056.Coin:Coin", "createArgument": {"amount": "2.5"}}},
  {"ExercisedEvent": {"templateId": "pkg:Wormhole.Ntt.Manager:NttManager", "choice": "Mint", "exerciseResult": {"amount": "250000000"}}}]}'

check_case "json_array of two values" 0 '[{"a":1},2]' - json_array '{"a": 1}' 2
check_case "created_contract_argument of one created event" 0 '{"amount":"2.5"}' - \
  created_contract_argument "$CREATED_COIN_TRANSACTION" ":Token.CIP0056.Coin:Coin"
check_case "created_contract_argument with no match" 5 - "found 0" \
  created_contract_argument "$CREATED_COIN_TRANSACTION" ":Wormhole.Ntt.Ledger:LockedLedger"

readonly MANAGER_TRANSFER_EVENT='{"ExercisedEvent": {"templateId": "pkg:Wormhole.Ntt.Manager:NttManager", "choice": "Transfer", "exerciseResult": {"sequence": "2"}}}'
readonly FACTORY_TRANSFER_EVENT='{"ExercisedEvent": {"templateId": "pkg:Splice.Api.Token.TransferInstructionV1:TransferFactory", "choice": "Transfer", "exerciseResult": {"output": "completed"}}}'
check_case "exercise_result with one match" 0 '{"sequence":"2"}' - \
  exercise_result "{\"events\": [$MANAGER_TRANSFER_EVENT]}" ":Wormhole.Ntt.Manager:NttManager" Transfer
check_case "exercise_result skips the same choice on another template" 0 '{"sequence":"2"}' - \
  exercise_result "{\"events\": [$FACTORY_TRANSFER_EVENT, $MANAGER_TRANSFER_EVENT]}" ":Wormhole.Ntt.Manager:NttManager" Transfer
check_case "exercise_result with two matches" 5 - "found 2" \
  exercise_result "{\"events\": [$MANAGER_TRANSFER_EVENT, $MANAGER_TRANSFER_EVENT]}" ":Wormhole.Ntt.Manager:NttManager" Transfer
check_case "exercise_result with no match" 5 - "found 0" \
  exercise_result "{\"events\": [$FACTORY_TRANSFER_EVENT]}" ":Wormhole.Ntt.Manager:NttManager" Transfer

readonly REPOSITORY_ROOT="$LOCALNET_TEST_DIR/../../.."
readonly VENDORED_DAR_DIR="$REPOSITORY_ROOT/canton/dars"
readonly CORE_0_6_0_PACKAGE_ID="701a57c867e02d962f231bd9f0ff5186e16462a77289aa1b7aaba7fe95074c15"
readonly CORE_0_7_0_PACKAGE_ID="7a22a1c4026b2a037b021063c4ff2bd5506be2e748aa9de8b7a6057c90c356eb"

# pinned_value_in_ntt_lib <readonly-name>: the value of that pin in ntt_lib.sh, which must define it exactly once.
pinned_value_in_ntt_lib() {
  local pinned_values
  pinned_values="$(sed -nE "s/^readonly $1=\"([^\"]+)\"\$/\\1/p" "$LOCALNET_TEST_DIR/../ntt_lib.sh")"
  if [[ "$(grep -c . <<<"$pinned_values")" -ne 1 ]]; then
    echo "ntt_lib.sh defines $1 $(grep -c . <<<"$pinned_values") times, expected 1" >&2
    return 1
  fi
  echo "$pinned_values"
}
pinned_core_package_id="$(pinned_value_in_ntt_lib WORMHOLE_CORE_PACKAGE_ID)" || failure_count=$((failure_count + 1))
pinned_core_version="$(pinned_value_in_ntt_lib WORMHOLE_CORE_VERSION)" || failure_count=$((failure_count + 1))
pinned_ntt_package_id="$(pinned_value_in_ntt_lib NTT_PACKAGE_ID)" || failure_count=$((failure_count + 1))
pinned_ntt_version="$(pinned_value_in_ntt_lib NTT_VERSION)" || failure_count=$((failure_count + 1))
check_case "pinned core id is 0.8.0 03a65dc3" 0 - - \
  test "$pinned_core_package_id" == "03a65dc33bc2e08ad04318e300227e3f4dc6b983be09b650b59f2676f43a22b6"
check_case "pinned core version" 0 - - test "$pinned_core_version" == "0.8.0"
check_case "pinned ntt id is 0.3.2 f77fe8a8" 0 - - \
  test "$pinned_ntt_package_id" == "f77fe8a8710039087e5d0184e7f6fb65bfdbaf23875196bf202087d855643e71"
check_case "pinned ntt version" 0 - - test "$pinned_ntt_version" == "0.3.2"

echo "not a zip archive" > "$work_dir/not-a-dar.dar"
check_case "core 0.7.0 main id" 0 "$CORE_0_7_0_PACKAGE_ID" - dar_main_package_id "$VENDORED_DAR_DIR/wormhole-core-0.7.0.dar"
check_case "core 0.6.0 main id" 0 "$CORE_0_6_0_PACKAGE_ID" - dar_main_package_id "$VENDORED_DAR_DIR/wormhole-core-0.6.0.dar"
check_case "core 0.7.0 main name and version" 0 "wormhole-core 0.7.0" - \
  dar_main_package_name_and_version "$VENDORED_DAR_DIR/wormhole-core-0.7.0.dar"
check_case "core id of the core 0.7.0 DAR" 0 - - \
  check_core_package_id_of_dar "$VENDORED_DAR_DIR/wormhole-core-0.7.0.dar" "$CORE_0_7_0_PACKAGE_ID"
check_case "wrong expected core id" 1 - "$CORE_0_7_0_PACKAGE_ID, expected $pinned_core_package_id" \
  check_core_package_id_of_dar "$VENDORED_DAR_DIR/wormhole-core-0.7.0.dar" "$pinned_core_package_id"
check_case "no core DALF" 1 - "found 0" \
  check_core_package_id_of_dar "$VENDORED_DAR_DIR/splice-api-token-holding-v1-1.0.0.dar" "$pinned_core_package_id"
check_case "main id of a file that is not a DAR" 1 - "cannot list" dar_main_package_id "$work_dir/not-a-dar.dar"
check_case "core id of a file that is not a DAR" 1 - "cannot list" \
  check_core_package_id_of_dar "$work_dir/not-a-dar.dar" "$pinned_core_package_id"
check_case "package ids of the core 0.7.0 DAR include its main id" 0 - - \
  check_text_has_line "$(dar_package_ids "$VENDORED_DAR_DIR/wormhole-core-0.7.0.dar")" "$CORE_0_7_0_PACKAGE_ID"

ntt_dar="${NTT_REPO_DIR:-$REPOSITORY_ROOT/../ntt}/canton/ntt/.daml/dist/ntt-$pinned_ntt_version.dar"
if [[ -f "$ntt_dar" ]]; then
  check_case "NTT DAR main id equals the pin" 0 "$pinned_ntt_package_id" - dar_main_package_id "$ntt_dar"
  check_case "NTT DAR main name and version" 0 "ntt $pinned_ntt_version" - dar_main_package_name_and_version "$ntt_dar"
  check_case "NTT DAR carries exactly the pinned core" 0 - - check_core_package_id_of_dar "$ntt_dar" "$pinned_core_package_id"
else
  echo "skip: $ntt_dar is absent; set NTT_REPO_DIR to an NTT checkout with built DARs"
fi
localnet_core_dar="$REPOSITORY_ROOT/canton/core/.daml/dist/wormhole-core-$pinned_core_version.dar"
if [[ -f "$localnet_core_dar" ]]; then
  check_case "LocalNet core DAR main id equals the pin" 0 "$pinned_core_package_id" - dar_main_package_id "$localnet_core_dar"
else
  echo "skip: $localnet_core_dar is absent; run 'dpm build --all' in canton"
fi

if [[ "$failure_count" -ne 0 ]]; then
  echo "ntt_lib_test.sh: $failure_count failures" >&2
  exit 1
fi
echo "ntt_lib_test.sh: PASS"
