# Input checks of the owner-key scripts. Source this file; do not run it. Needs only bash and jq.
# shellcheck shell=bash

# Rehearsal fault and measurement variables of gg_rotate_owner_key.sh (README "Owner key rotation faults").
readonly ROTATION_FAULT_VARIABLES=(
  GG_ROTATION_FAULT_SIGNER_OF
  GG_ROTATION_FAULT_OMIT_PARTY_OF
  GG_ROTATION_FAULT_UNSIGNED_BY_ADDED_OF
  GG_ROTATION_MEASURE_SINGLE_LOAD
  GG_ROTATION_FAULT_PARTY_IDS
)

# check_owner_names_are_safe <owner-name>...: each name is a key file prefix of [A-Za-z0-9_-] only.
check_owner_names_are_safe() {
  local owner_name
  if [[ $# -eq 0 ]]; then
    echo "no owner names" >&2
    return 2
  fi
  for owner_name in "$@"; do
    if [[ ! "$owner_name" =~ ^[A-Za-z0-9_-]+$ ]]; then
      echo "owner name has characters outside [A-Za-z0-9_-]: $owner_name" >&2
      return 2
    fi
  done
}

# check_owner_names_differ <owner-name>...: no name occurs twice.
check_owner_names_differ() {
  if [[ "$(printf '%s\n' "$@" | sort -u | wc -l | tr -d ' ')" != "$#" ]]; then
    echo "the $# owner names must differ: $*" >&2
    return 2
  fi
}

# read_custody_party_id <custody-artifacts-file>: prints the partyId, or nothing when the file does not exist.
# Fails when the file exists without a <name>::<namespace fingerprint> partyId.
read_custody_party_id() {
  local custody_artifacts_file="$1" custody_party_id
  if [[ ! -e "$custody_artifacts_file" ]]; then
    return 0
  fi
  if ! custody_party_id="$(jq -er '.partyId | strings' "$custody_artifacts_file" 2>/dev/null)"; then
    echo "$custody_artifacts_file has no partyId. Run custody_host.sh again, or remove the file." >&2
    return 1
  fi
  if [[ ! "$custody_party_id" =~ ^[A-Za-z][A-Za-z0-9_-]*::1220[0-9a-f]{64}$ ]]; then
    echo "partyId $custody_party_id in $custody_artifacts_file is not <name>::1220<64 hex>" >&2
    return 1
  fi
  echo "$custody_party_id"
}

# custody_artifacts_file_name <party-name>: artifacts file name of a custody party of the guardian namespace.
custody_artifacts_file_name() {
  if [[ $# -ne 1 ]]; then
    echo "usage: custody_artifacts_file_name <party-name>" >&2
    return 2
  fi
  case "$1" in
    nttCustody) echo "custody-artifacts.json" ;;
    feeCustody) echo "fee-custody-artifacts.json" ;;
    *)
      echo "unknown custody party name: $1" >&2
      return 2
      ;;
  esac
}

# rotation_party_ids_in_load_order <ntt-custody-id> <fee-custody-id> <guardian-governance-id>: comma-separated party
# ids in load order. Custody parties first, guardianGovernance last. An empty custody id is left out.
rotation_party_ids_in_load_order() {
  if [[ $# -ne 3 ]]; then
    echo "usage: rotation_party_ids_in_load_order <ntt-custody-id> <fee-custody-id> <guardian-governance-id>" >&2
    return 2
  fi
  if [[ -z "$3" ]]; then
    echo "guardianGovernance party id is empty" >&2
    return 2
  fi
  local party_id party_ids=()
  for party_id in "$@"; do
    if [[ -n "$party_id" ]]; then
      party_ids+=("$party_id")
    fi
  done
  (IFS=,; echo "${party_ids[*]}")
}

# check_fee_custody_is_in_the_guardian_namespace <fee-custody-id> <guardian-governance-id> <ntt-custody-id-or-empty>:
# feeCustody has the name feeCustody, the namespace of guardianGovernance, and is not nttCustody.
# Daml cannot read a party namespace, so the genesis ceremony checks it.
check_fee_custody_is_in_the_guardian_namespace() {
  if [[ $# -ne 3 ]]; then
    echo "usage: check_fee_custody_is_in_the_guardian_namespace <fee-custody-id> <guardian-governance-id> <ntt-custody-id-or-empty>" >&2
    return 2
  fi
  local fee_custody_id="$1" guardian_governance_id="$2" ntt_custody_id="$3"
  if [[ -z "$fee_custody_id" ]]; then
    echo "feeCustody party id is empty" >&2
    return 1
  fi
  if [[ "${fee_custody_id%%::*}" != "feeCustody" ]]; then
    echo "fee custody party $fee_custody_id is not named feeCustody" >&2
    return 1
  fi
  if [[ "${fee_custody_id#*::}" != "${guardian_governance_id#*::}" ]]; then
    echo "feeCustody $fee_custody_id is outside the namespace of $guardian_governance_id" >&2
    return 1
  fi
  if [[ "$fee_custody_id" == "$ntt_custody_id" ]]; then
    echo "feeCustody and nttCustody are the same party: $fee_custody_id" >&2
    return 1
  fi
}

# check_core_state_pays_fees_to_fee_custody <core-state-created-event-json> <operator-id> <guardian-governance-id>
# <fee-custody-id>: feeRecipient is feeCustody, the signatories are exactly operator, guardianGovernance and
# feeCustody, and the fee instrument admin is neither guardianGovernance nor operator.
check_core_state_pays_fees_to_fee_custody() {
  if [[ $# -ne 4 ]]; then
    echo "usage: check_core_state_pays_fees_to_fee_custody <core-state-created-event-json> <operator-id> <guardian-governance-id> <fee-custody-id>" >&2
    return 2
  fi
  local core_state_event="$1" operator_id="$2" guardian_governance_id="$3" fee_custody_id="$4"
  local fee_recipient fee_instrument_admin signatories expected_signatories
  if ! fee_recipient="$(jq -er '.createArgument.feeRecipient | strings' <<<"$core_state_event" 2>/dev/null)" \
      || ! fee_instrument_admin="$(jq -er '.createArgument.feeInstrument.admin | strings' <<<"$core_state_event" 2>/dev/null)" \
      || ! signatories="$(jq -ce '.signatories | sort' <<<"$core_state_event" 2>/dev/null)"; then
    echo "CoreState created event is not readable" >&2
    return 1
  fi
  if [[ "$fee_recipient" != "$fee_custody_id" ]]; then
    echo "CoreState feeRecipient is $fee_recipient, expected $fee_custody_id" >&2
    return 1
  fi
  expected_signatories="$(jq -cn '$ARGS.positional | sort' --args "$operator_id" "$guardian_governance_id" "$fee_custody_id")"
  if [[ "$signatories" != "$expected_signatories" ]]; then
    echo "CoreState signatories $signatories, expected $expected_signatories" >&2
    return 1
  fi
  if [[ "$fee_instrument_admin" == "$guardian_governance_id" || "$fee_instrument_admin" == "$operator_id" ]]; then
    echo "CoreState fee instrument admin is $fee_instrument_admin" >&2
    return 1
  fi
}

# print_active_rotation_faults: one line for each rotation fault variable with a non-empty value.
print_active_rotation_faults() {
  local fault_variable
  for fault_variable in "${ROTATION_FAULT_VARIABLES[@]}"; do
    if [[ -n "${!fault_variable:-}" ]]; then
      echo "ROTATION FAULT INJECTION ACTIVE: $fault_variable=${!fault_variable}"
    fi
  done
}
