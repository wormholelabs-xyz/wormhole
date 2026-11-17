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

# print_active_rotation_faults: one line for each rotation fault variable with a non-empty value.
print_active_rotation_faults() {
  local fault_variable
  for fault_variable in "${ROTATION_FAULT_VARIABLES[@]}"; do
    if [[ -n "${!fault_variable:-}" ]]; then
      echo "ROTATION FAULT INJECTION ACTIVE: $fault_variable=${!fault_variable}"
    fi
  done
}
