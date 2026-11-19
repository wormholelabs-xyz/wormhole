#!/usr/bin/env bash
# Check that guardianGovernance and one custody party have distinct Daml authority, although one 2-of-3 key set signs
# for both. Each case creates GovernanceCeremonyProbe (single signatory) through an interactive submission.
#   1. actAs custody, custody signatures, signatory custody. Expect commit.
#   2. actAs guardianGovernance, guardianGovernance signatures, signatory custody. Expect DAML_AUTHORIZATION_ERROR.
#   3. actAs custody, custody signatures, signatory guardianGovernance. Expect DAML_AUTHORIZATION_ERROR.
#   4. actAs custody, guardianGovernance signatures only. Expect FAILED_TO_EXECUTE_TRANSACTION (no custody signature).
#   5. actAs both parties, one owner pair signs for each, signatory custody. Expect commit (joint ceremony).
#   6. actAs custody, one owner key signs. Expect FAILED_TO_EXECUTE_TRANSACTION (1 valid signature of 2).
#   7. actAs guardianGovernance, one owner key signs. Expect FAILED_TO_EXECUTE_TRANSACTION (1 valid signature of 2).
#   8. When the other custody party exists: actAs custody, custody signatures, signatory the other custody party.
#      Expect DAML_AUTHORIZATION_ERROR.
# Usage: custody_authority_check.sh [--party <custody-party-name>] [<owner> <owner>]
#   --party: nttCustody (default) or feeCustody. Two current owner names (default owner1 owner2).
# Precondition: custody_host.sh for that party and gg_package_vetting.sh ran.
set -euo pipefail
# shellcheck source=gg_lib.sh
source "$(dirname "${BASH_SOURCE[0]}")/gg_lib.sh"

readonly USAGE="usage: custody_authority_check.sh [--party <custody-party-name>] [<owner> <owner>]"
custody_party_name=nttCustody
if [[ "${1:-}" == "--party" ]]; then
  custody_party_name="${2:?$USAGE}"
  shift 2
fi
if [[ $# -ne 0 && $# -ne 2 ]]; then
  echo "$USAGE" >&2
  exit 2
fi
case "$custody_party_name" in
  nttCustody) other_custody_party_name=feeCustody ;;
  feeCustody) other_custody_party_name=nttCustody ;;
  *)
    echo "unknown custody party name: $custody_party_name" >&2
    exit 2
    ;;
esac
CHECKED_CUSTODY_PARTY="$(custody_party_id_of "$custody_party_name")"
OTHER_CUSTODY_PARTY="$(custody_party_id_of "$other_custody_party_name")"
readonly CHECKED_CUSTODY_PARTY OTHER_CUSTODY_PARTY
if [[ -z "$CHECKED_CUSTODY_PARTY" ]]; then
  echo "Missing the artifacts of $custody_party_name. Run CUSTODY_PARTY_NAME=$custody_party_name custody_host.sh first." >&2
  exit 1
fi
signing_owners=("${1:-owner1}" "${2:-owner2}")
check_owner_names_are_safe "${signing_owners[@]}"
check_owner_names_differ "${signing_owners[@]}"

party_json_array() { jq -cn '$ARGS.positional' --args "$@"; }
custody_signers="$(namespace_party_signers_json "$CHECKED_CUSTODY_PARTY" "${signing_owners[@]}" | jq -c .)"
gg_signers="$(gg_signers_json "${signing_owners[@]}" | jq -c .)"
joint_signers="$(jq -cs add <<<"$custody_signers $gg_signers")"

readonly CASE_SEPARATOR=$'\x1f'
readonly EXPECTED_TEXT_SEPARATOR=$'\x1e'
# authority_case <name> <act-as-json> <signatory> <signers-json> <expected>: one table row. <expected> is commit, or
# the error texts that the rejection must contain, joined with EXPECTED_TEXT_SEPARATOR.
authority_case() {
  local IFS="$CASE_SEPARATOR"
  echo "$*"
}
custody_only="$(party_json_array "$CHECKED_CUSTODY_PARTY")"
gg_only="$(party_json_array "$GG_PARTY")"
# one_key_rejection <party-id>: the error texts of a submission with one valid owner signature for that party.
one_key_rejection() {
  printf '%s%s%s' FAILED_TO_EXECUTE_TRANSACTION "$EXPECTED_TEXT_SEPARATOR" \
    "Received 1 valid signatures from distinct keys (0 invalid), but expected at least 2 valid for ${1%%::*}::"
}
authority_cases=(
  "$(authority_case 1-custody-signs-for-custody "$custody_only" "$CHECKED_CUSTODY_PARTY" "$custody_signers" commit)"
  "$(authority_case 2-gg-creates-custody-contract "$gg_only" "$CHECKED_CUSTODY_PARTY" "$gg_signers" DAML_AUTHORIZATION_ERROR)"
  "$(authority_case 3-custody-creates-gg-contract "$custody_only" "$GG_PARTY" "$custody_signers" DAML_AUTHORIZATION_ERROR)"
  "$(authority_case 4-gg-signatures-for-custody "$custody_only" "$CHECKED_CUSTODY_PARTY" "$gg_signers" \
    "did not provide an external signature: $CHECKED_CUSTODY_PARTY")"
  "$(authority_case 5-joint-ceremony "$(party_json_array "$GG_PARTY" "$CHECKED_CUSTODY_PARTY")" "$CHECKED_CUSTODY_PARTY" "$joint_signers" commit)"
  "$(authority_case 6-one-key-for-custody "$custody_only" "$CHECKED_CUSTODY_PARTY" \
    "$(namespace_party_signers_json "$CHECKED_CUSTODY_PARTY" "${signing_owners[0]}" | jq -c .)" "$(one_key_rejection "$CHECKED_CUSTODY_PARTY")")"
  "$(authority_case 7-one-key-for-gg "$gg_only" "$GG_PARTY" \
    "$(gg_signers_json "${signing_owners[0]}" | jq -c .)" "$(one_key_rejection "$GG_PARTY")")"
)
if [[ -n "$OTHER_CUSTODY_PARTY" ]]; then
  authority_cases+=("$(authority_case "8-$custody_party_name-creates-$other_custody_party_name-contract" "$custody_only" \
    "$OTHER_CUSTODY_PARTY" "$custody_signers" DAML_AUTHORIZATION_ERROR)")
fi
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
    IFS="$EXPECTED_TEXT_SEPARATOR" read -r -a expected_texts <<<"$expected"
    for expected_text in "${expected_texts[@]}"; do
      if ! grep -qF -- "$expected_text" "$gg_work_dir/case.err"; then
        echo "FAIL: expected rejection with: $expected_text" >&2
        cat "$gg_work_dir/case.err" >&2
        exit 1
      fi
    done
    echo "  $(grep -oE '"cause":"[^"]*"' "$gg_work_dir/case.err" | head -n 1)"
    echo "PASS: rejected with ${expected_texts[*]}"
  fi
done
echo "PASS: all ${#authority_cases[@]} authority cases"
