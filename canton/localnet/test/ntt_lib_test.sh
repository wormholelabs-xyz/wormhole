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

readonly CREATED_COIN_TRANSACTION='{"events": [
  {"CreatedEvent": {"templateId": "pkg:Token.CIP0056.Coin:Coin", "createArgument": {"amount": "2.5"}}},
  {"ExercisedEvent": {"templateId": "pkg:Wormhole.Ntt.Manager:NttManager", "choice": "Mint", "exerciseResult": {"amount": "250000000"}}}]}'

check_case "json_array of two values" 0 '[{"a":1},2]' - json_array '{"a": 1}' 2
check_case "created_contract_argument of one created event" 0 '{"amount":"2.5"}' - \
  created_contract_argument "$CREATED_COIN_TRANSACTION" ":Token.CIP0056.Coin:Coin"
check_case "created_contract_argument with no match" 5 - "found 0" \
  created_contract_argument "$CREATED_COIN_TRANSACTION" ":Wormhole.Ntt.Ledger:LockedLedger"

if [[ "$failure_count" -ne 0 ]]; then
  echo "ntt_lib_test.sh: $failure_count failures" >&2
  exit 1
fi
echo "ntt_lib_test.sh: PASS"
