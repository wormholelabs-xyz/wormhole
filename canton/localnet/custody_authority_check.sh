#!/usr/bin/env bash
# Check that guardianGovernance and nttCustody have distinct Daml authority, although one 2-of-3 key set signs for both.
# Each case creates GovernanceCeremonyProbe (single signatory) through an interactive submission.
#   1. actAs nttCustody, nttCustody signatures, signatory nttCustody. Expect commit.
#   2. actAs guardianGovernance, guardianGovernance signatures, signatory nttCustody. Expect DAML_AUTHORIZATION_ERROR.
#   3. actAs nttCustody, nttCustody signatures, signatory guardianGovernance. Expect DAML_AUTHORIZATION_ERROR.
#   4. actAs nttCustody, guardianGovernance signatures only. Expect FAILED_TO_EXECUTE_TRANSACTION (no nttCustody signature).
#   5. actAs both parties, one owner pair signs for each, signatory nttCustody. Expect commit (joint ceremony).
# Usage: custody_authority_check.sh [<owner> <owner>]   Two current owner names (default owner1 owner2).
# Precondition: custody_host.sh and gg_package_vetting.sh ran.
set -euo pipefail
# shellcheck source=gg_lib.sh
source "$(dirname "${BASH_SOURCE[0]}")/gg_lib.sh"

if [[ -z "$CUSTODY_PARTY" ]]; then
  echo "Missing $CUSTODY_ARTIFACTS. Run custody_host.sh first." >&2
  exit 1
fi
if [[ $# -ne 0 && $# -ne 2 ]]; then
  echo "usage: custody_authority_check.sh [<owner> <owner>]" >&2
  exit 2
fi
signing_owners=("${1:-owner1}" "${2:-owner2}")
if [[ "${signing_owners[0]}" == "${signing_owners[1]}" ]]; then
  echo "the two owner names must differ" >&2
  exit 2
fi

party_json_array() { jq -cn '$ARGS.positional' --args "$@"; }
custody_signers="$(namespace_party_signers_json "$CUSTODY_PARTY" "${signing_owners[@]}" | jq -c .)"
gg_signers="$(gg_signers_json "${signing_owners[@]}" | jq -c .)"
joint_signers="$(jq -cs add <<<"$custody_signers $gg_signers")"

readonly CASE_SEPARATOR=$'\x1f'
# authority_case <name> <act-as-json> <signatory> <signers-json> <expected: commit or an error text>: one table row.
authority_case() {
  local IFS="$CASE_SEPARATOR"
  echo "$*"
}
custody_only="$(party_json_array "$CUSTODY_PARTY")"
gg_only="$(party_json_array "$GG_PARTY")"
authority_cases=(
  "$(authority_case 1-custody-signs-for-custody "$custody_only" "$CUSTODY_PARTY" "$custody_signers" commit)"
  "$(authority_case 2-gg-creates-custody-contract "$gg_only" "$CUSTODY_PARTY" "$gg_signers" DAML_AUTHORIZATION_ERROR)"
  "$(authority_case 3-custody-creates-gg-contract "$custody_only" "$GG_PARTY" "$custody_signers" DAML_AUTHORIZATION_ERROR)"
  "$(authority_case 4-gg-signatures-for-custody "$custody_only" "$CUSTODY_PARTY" "$gg_signers" \
    "did not provide an external signature: $CUSTODY_PARTY")"
  "$(authority_case 5-joint-ceremony "$(party_json_array "$GG_PARTY" "$CUSTODY_PARTY")" "$CUSTODY_PARTY" "$joint_signers" commit)"
)
for authority_case in "${authority_cases[@]}"; do
  IFS="$CASE_SEPARATOR" read -r case_name act_as signatory signers expected <<<"$authority_case"
  act_as_names="$(jq -r 'map(split("::")[0]) | join("+")' <<<"$act_as")"
  signer_party_names="$(jq -r 'keys | map(split("::")[0]) | join("+")' <<<"$signers")"
  echo "Case $case_name: actAs $act_as_names, signatory ${signatory%%::*}, signatures for $signer_party_names"
  submission_status=0
  submit_interactive "custody-$case_name-$(date +%s)" "$act_as" "$(ceremony_probe_create_command "$signatory" "$case_name")" \
    "$signers" > "$gg_work_dir/case.out" 2> "$gg_work_dir/case.err" || submission_status=$?
  if [[ "$expected" == commit ]]; then
    if [[ "$submission_status" -ne 0 ]]; then
      echo "FAIL: expected commit" >&2
      cat "$gg_work_dir/case.err" >&2
      exit 1
    fi
    created_signatories="$(jq -c '[.events[] | .CreatedEvent? // empty | .signatories] | add' "$gg_work_dir/case.out")"
    if [[ "$created_signatories" != "$(party_json_array "$signatory")" ]]; then
      echo "FAIL: created contract signatories $created_signatories, expected [$signatory]" >&2
      exit 1
    fi
    echo "PASS: committed, update $(jq -r .updateId "$gg_work_dir/case.out"), signatories $created_signatories"
  else
    if [[ "$submission_status" -eq 0 ]]; then
      echo "FAIL: committed, expected rejection with: $expected" >&2
      exit 1
    fi
    if ! grep -qF -- "$expected" "$gg_work_dir/case.err"; then
      echo "FAIL: expected rejection with: $expected" >&2
      cat "$gg_work_dir/case.err" >&2
      exit 1
    fi
    echo "  $(grep -oE '"cause":"[^"]*"' "$gg_work_dir/case.err" | head -n 1)"
    echo "PASS: rejected with $expected"
  fi
done
echo "PASS: all ${#authority_cases[@]} authority cases"
