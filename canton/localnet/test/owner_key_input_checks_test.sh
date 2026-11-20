#!/usr/bin/env bash
# Table test of owner_key_input_checks.sh: owner names, the custody artifacts files, the rotation load order, the
# feeCustody genesis checks and the rotation fault banner.
# Needs no Docker and no LocalNet.
# Usage: canton/localnet/test/owner_key_input_checks_test.sh
set -euo pipefail
LOCALNET_TEST_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=SCRIPTDIR/../owner_key_input_checks.sh
source "$LOCALNET_TEST_DIR/../owner_key_input_checks.sh"

work_dir="$(mktemp -d)"
trap 'rm -rf "$work_dir"' EXIT
VALID_PARTY_ID="nttCustody::1220$(printf 'ab%.0s' $(seq 1 32))"
readonly VALID_PARTY_ID
failed_cases=0
case_count=0

# check_case <name> <expected status> <expected output text or empty> <command>...: stdout and stderr together.
check_case() {
  local case_name="$1" expected_status="$2" expected_text="$3" actual_status=0 actual_output
  shift 3
  case_count=$((case_count + 1))
  actual_output="$("$@" 2>&1)" || actual_status=$?
  if [[ "$actual_status" -ne "$expected_status" ]]; then
    echo "FAIL $case_name: status $actual_status, expected $expected_status. Output: $actual_output"
    failed_cases=$((failed_cases + 1))
  elif [[ -z "$expected_text" && -n "$actual_output" ]]; then
    echo "FAIL $case_name: expected no output, got: $actual_output"
    failed_cases=$((failed_cases + 1))
  elif [[ -n "$expected_text" && "$actual_output" != *"$expected_text"* ]]; then
    echo "FAIL $case_name: output does not contain '$expected_text': $actual_output"
    failed_cases=$((failed_cases + 1))
  else
    echo "PASS $case_name"
  fi
}

check_case names-safe 0 "" check_owner_names_are_safe owner1 owner2 owner3
check_case names-with-dash-and-underscore 0 "" check_owner_names_are_safe owner-r1_x owner2
check_case name-with-space 2 "characters outside [A-Za-z0-9_-]: own er1" check_owner_names_are_safe owner1 "own er1"
check_case name-with-semicolon 2 "characters outside [A-Za-z0-9_-]: owner;1" check_owner_names_are_safe "owner;1"
check_case name-with-path 2 "characters outside [A-Za-z0-9_-]: ../owner1" check_owner_names_are_safe ../owner1
check_case name-empty 2 "characters outside [A-Za-z0-9_-]: " check_owner_names_are_safe owner1 ""
check_case names-none 2 "no owner names" check_owner_names_are_safe

check_case names-differ 0 "" check_owner_names_differ owner1 owner2 owner3
check_case four-names-differ 0 "" check_owner_names_differ owner3 owner1 owner2 owner4
check_case names-repeat 2 "the 3 owner names must differ" check_owner_names_differ owner1 owner1 owner2
check_case names-repeat-last 2 "the 4 owner names must differ" check_owner_names_differ owner1 owner2 owner3 owner1

printf '{"partyId": "%s"}\n' "$VALID_PARTY_ID" > "$work_dir/valid.json"
printf '{"hostingParticipants": []}\n' > "$work_dir/no-party.json"
printf '{"partyId": "nttCustody"}\n' > "$work_dir/malformed-party.json"
printf 'not json\n' > "$work_dir/not-json.json"
check_case custody-file-absent 0 "" read_custody_party_id "$work_dir/absent.json"
check_case custody-file-valid 0 "$VALID_PARTY_ID" read_custody_party_id "$work_dir/valid.json"
check_case custody-file-without-party 1 "$work_dir/no-party.json has no partyId" read_custody_party_id "$work_dir/no-party.json"
check_case custody-file-malformed-party 1 "is not <name>::1220<64 hex>" read_custody_party_id "$work_dir/malformed-party.json"
check_case custody-file-not-json 1 "$work_dir/not-json.json has no partyId" read_custody_party_id "$work_dir/not-json.json"

check_case artifacts-of-ntt-custody 0 "custody-artifacts.json" custody_artifacts_file_name nttCustody
check_case artifacts-of-fee-custody 0 "fee-custody-artifacts.json" custody_artifacts_file_name feeCustody
check_case artifacts-of-guardian-governance 2 "unknown custody party name: guardianGovernance" \
  custody_artifacts_file_name guardianGovernance
check_case artifacts-of-empty-name 2 "unknown custody party name: " custody_artifacts_file_name ""
check_case artifacts-without-name 2 "usage: custody_artifacts_file_name <party-name>" custody_artifacts_file_name

FEE_CUSTODY_PARTY_ID="feeCustody::1220$(printf 'ab%.0s' $(seq 1 32))"
GG_PARTY_ID="guardianGovernance::1220$(printf 'ab%.0s' $(seq 1 32))"
check_case load-order-all-parties 0 "$VALID_PARTY_ID,$FEE_CUSTODY_PARTY_ID,$GG_PARTY_ID" \
  rotation_party_ids_in_load_order "$VALID_PARTY_ID" "$FEE_CUSTODY_PARTY_ID" "$GG_PARTY_ID"
check_case load-order-without-ntt-custody 0 "$FEE_CUSTODY_PARTY_ID,$GG_PARTY_ID" \
  rotation_party_ids_in_load_order "" "$FEE_CUSTODY_PARTY_ID" "$GG_PARTY_ID"
check_case load-order-guardian-governance-only 0 "$GG_PARTY_ID" rotation_party_ids_in_load_order "" "" "$GG_PARTY_ID"
check_case load-order-without-guardian-governance 2 "guardianGovernance party id is empty" \
  rotation_party_ids_in_load_order "$VALID_PARTY_ID" "$FEE_CUSTODY_PARTY_ID" ""
check_case load-order-two-arguments 2 "usage: rotation_party_ids_in_load_order" \
  rotation_party_ids_in_load_order "$VALID_PARTY_ID" "$GG_PARTY_ID"

OTHER_NAMESPACE_FEE_CUSTODY_PARTY_ID="feeCustody::1220$(printf 'cd%.0s' $(seq 1 32))"
check_case fee-custody-in-namespace 0 "" \
  check_fee_custody_is_in_the_guardian_namespace "$FEE_CUSTODY_PARTY_ID" "$GG_PARTY_ID" "$VALID_PARTY_ID"
check_case fee-custody-without-ntt-custody 0 "" \
  check_fee_custody_is_in_the_guardian_namespace "$FEE_CUSTODY_PARTY_ID" "$GG_PARTY_ID" ""
check_case fee-custody-empty 1 "feeCustody party id is empty" \
  check_fee_custody_is_in_the_guardian_namespace "" "$GG_PARTY_ID" "$VALID_PARTY_ID"
check_case fee-custody-is-ntt-custody 1 "is not named feeCustody" \
  check_fee_custody_is_in_the_guardian_namespace "$VALID_PARTY_ID" "$GG_PARTY_ID" "$VALID_PARTY_ID"
check_case fee-custody-is-guardian-governance 1 "is not named feeCustody" \
  check_fee_custody_is_in_the_guardian_namespace "$GG_PARTY_ID" "$GG_PARTY_ID" ""
check_case fee-custody-other-namespace 1 "is outside the namespace of $GG_PARTY_ID" \
  check_fee_custody_is_in_the_guardian_namespace "$OTHER_NAMESPACE_FEE_CUSTODY_PARTY_ID" "$GG_PARTY_ID" ""
check_case fee-custody-same-as-ntt-custody-id 1 "feeCustody and nttCustody are the same party" \
  check_fee_custody_is_in_the_guardian_namespace "$FEE_CUSTODY_PARTY_ID" "$GG_PARTY_ID" "$FEE_CUSTODY_PARTY_ID"
check_case fee-custody-two-arguments 2 "usage: check_fee_custody_is_in_the_guardian_namespace" \
  check_fee_custody_is_in_the_guardian_namespace "$FEE_CUSTODY_PARTY_ID" "$GG_PARTY_ID"

FORMER_OPERATOR_PARTY_ID="Operator::1220$(printf 'ef%.0s' $(seq 1 32))"
DSO_PARTY_ID="DSO::1220$(printf '01%.0s' $(seq 1 32))"
# core_state_event <fee-recipient> <fee-instrument-admin> <signatory>...: a CoreState created event.
core_state_event() {
  local fee_recipient="$1" fee_instrument_admin="$2"
  shift 2
  jq -cn --arg feeRecipient "$fee_recipient" --arg admin "$fee_instrument_admin" '{
    createArgument: {feeRecipient: $feeRecipient, feeInstrument: {admin: $admin, id: "Amulet"}},
    signatories: $ARGS.positional}' --args "$@"
}
check_case core-state-pays-fee-custody 0 "" check_core_state_pays_fees_to_fee_custody \
  "$(core_state_event "$FEE_CUSTODY_PARTY_ID" "$DSO_PARTY_ID" "$GG_PARTY_ID" "$FEE_CUSTODY_PARTY_ID")" \
  "$GG_PARTY_ID" "$FEE_CUSTODY_PARTY_ID"
check_case core-state-signatories-in-other-order 0 "" check_core_state_pays_fees_to_fee_custody \
  "$(core_state_event "$FEE_CUSTODY_PARTY_ID" "$DSO_PARTY_ID" "$FEE_CUSTODY_PARTY_ID" "$GG_PARTY_ID")" \
  "$GG_PARTY_ID" "$FEE_CUSTODY_PARTY_ID"
check_case core-state-pays-guardian-governance 1 "CoreState feeRecipient is $GG_PARTY_ID, expected $FEE_CUSTODY_PARTY_ID" \
  check_core_state_pays_fees_to_fee_custody \
  "$(core_state_event "$GG_PARTY_ID" "$DSO_PARTY_ID" "$GG_PARTY_ID" "$FEE_CUSTODY_PARTY_ID")" \
  "$GG_PARTY_ID" "$FEE_CUSTODY_PARTY_ID"
check_case core-state-without-fee-custody-signatory 1 "CoreState signatories" check_core_state_pays_fees_to_fee_custody \
  "$(core_state_event "$FEE_CUSTODY_PARTY_ID" "$DSO_PARTY_ID" "$GG_PARTY_ID")" \
  "$GG_PARTY_ID" "$FEE_CUSTODY_PARTY_ID"
check_case core-state-with-former-operator-signatory 1 "CoreState signatories" check_core_state_pays_fees_to_fee_custody \
  "$(core_state_event "$FEE_CUSTODY_PARTY_ID" "$DSO_PARTY_ID" "$GG_PARTY_ID" "$FORMER_OPERATOR_PARTY_ID" "$FEE_CUSTODY_PARTY_ID")" \
  "$GG_PARTY_ID" "$FEE_CUSTODY_PARTY_ID"
check_case core-state-gg-fee-instrument-admin 1 "CoreState fee instrument admin is $GG_PARTY_ID" \
  check_core_state_pays_fees_to_fee_custody \
  "$(core_state_event "$FEE_CUSTODY_PARTY_ID" "$GG_PARTY_ID" "$GG_PARTY_ID" "$FEE_CUSTODY_PARTY_ID")" \
  "$GG_PARTY_ID" "$FEE_CUSTODY_PARTY_ID"
check_case core-state-fee-custody-fee-instrument-admin 1 "CoreState fee instrument admin is $FEE_CUSTODY_PARTY_ID" \
  check_core_state_pays_fees_to_fee_custody \
  "$(core_state_event "$FEE_CUSTODY_PARTY_ID" "$FEE_CUSTODY_PARTY_ID" "$GG_PARTY_ID" "$FEE_CUSTODY_PARTY_ID")" \
  "$GG_PARTY_ID" "$FEE_CUSTODY_PARTY_ID"
check_case core-state-not-json 1 "CoreState created event is not readable" \
  check_core_state_pays_fees_to_fee_custody "not json" "$GG_PARTY_ID" "$FEE_CUSTODY_PARTY_ID"
check_case core-state-four-arguments 2 "usage: check_core_state_pays_fees_to_fee_custody" \
  check_core_state_pays_fees_to_fee_custody "{}" "$FORMER_OPERATOR_PARTY_ID" "$GG_PARTY_ID" "$FEE_CUSTODY_PARTY_ID"

check_case fault-banner-none 0 "" env -u GG_ROTATION_FAULT_SIGNER_OF -u GG_ROTATION_FAULT_OMIT_PARTY_OF \
  -u GG_ROTATION_FAULT_UNSIGNED_BY_ADDED_OF -u GG_ROTATION_MEASURE_SINGLE_LOAD bash -c \
  "source '$LOCALNET_TEST_DIR/../owner_key_input_checks.sh' && print_active_rotation_faults"
check_case fault-banner-one 0 "ROTATION FAULT INJECTION ACTIVE: GG_ROTATION_FAULT_OMIT_PARTY_OF=nttCustody" \
  env GG_ROTATION_FAULT_OMIT_PARTY_OF=nttCustody bash -c \
  "source '$LOCALNET_TEST_DIR/../owner_key_input_checks.sh' && print_active_rotation_faults"
check_case fault-banner-empty-value 0 "" env GG_ROTATION_FAULT_SIGNER_OF= bash -c \
  "source '$LOCALNET_TEST_DIR/../owner_key_input_checks.sh' && print_active_rotation_faults"
check_case fault-banner-party-ids 0 "ROTATION FAULT INJECTION ACTIVE: GG_ROTATION_FAULT_PARTY_IDS=guardianGovernance::1220ab" \
  env GG_ROTATION_FAULT_PARTY_IDS=guardianGovernance::1220ab bash -c \
  "source '$LOCALNET_TEST_DIR/../owner_key_input_checks.sh' && print_active_rotation_faults"
check_case fault-banner-lists-each 0 "GG_ROTATION_MEASURE_SINGLE_LOAD=true" \
  env GG_ROTATION_FAULT_SIGNER_OF=owner4 GG_ROTATION_MEASURE_SINGLE_LOAD=true bash -c \
  "source '$LOCALNET_TEST_DIR/../owner_key_input_checks.sh' && print_active_rotation_faults | grep -c '^ROTATION FAULT INJECTION ACTIVE' | grep -qx 2 && print_active_rotation_faults"

if [[ "$failed_cases" -ne 0 ]]; then
  echo "FAIL: $failed_cases of $case_count cases"
  exit 1
fi
echo "PASS: $case_count cases"
