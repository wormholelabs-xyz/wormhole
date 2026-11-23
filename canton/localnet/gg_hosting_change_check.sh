#!/usr/bin/env bash
# Check the end state of the guardian replacement rehearsal (L1): guardianGovernance, feeCustody and nttCustody are
# hosted on sv, app-provider and guardian4 at Confirmation with threshold 2, as each participant sees it. The ACS of
# each party on guardian4 equals the ACS on its replication source (REPLICATION_SOURCE_OF_PARTY).
# Usage: gg_hosting_change_check.sh
set -euo pipefail
# shellcheck source=gg_lib.sh
source "$(dirname "${BASH_SOURCE[0]}")/gg_lib.sh"

readonly EXPECTED_HOSTS="app-provider/Confirmation,guardian4/Confirmation,sv/Confirmation"
readonly CHECKED_VIEWS=(sv app-provider guardian4)
# party name -> replication source, as in party_replication.sh.
declare -rA REPLICATION_SOURCE_OF_PARTY=([guardianGovernance]=app-provider [feeCustody]=app-provider [nttCustody]=sv)
readonly CHECKED_PARTIES=(guardianGovernance feeCustody nttCustody)
[[ -n "$FEE_CUSTODY_PARTY" ]] || fail "feeCustody does not exist; run custody_host.sh"
[[ -n "$CUSTODY_PARTY" ]] || fail "nttCustody does not exist; run custody_host.sh"
check_failures=0

report_check_outcome() {
  local description="$1"
  shift
  if "$@"; then
    echo "PASS: $description"
  else
    echo "FAIL: $description"
    check_failures=$((check_failures + 1))
  fi
}

hosting_output="$(LEDGER_API_TOKEN="$("$LOCALNET_SCRIPT_DIR/token.sh" "$SCRIPT_USER_ID")" HOSTING_PARTY_IDS="$GG_PARTY,$FEE_CUSTODY_PARTY,$CUSTODY_PARTY" HOSTING_ACS_DIGEST=true \
  CANTON_CONSOLE_CONFIG="$LOCALNET_SCRIPT_DIR/console/remote-participants-guardian4.conf" \
  "$LOCALNET_SCRIPT_DIR/canton_console.sh" "$LOCALNET_SCRIPT_DIR/console/hosting_topology.canton" 2>&1)" || true
grep -E '^(HOSTING|ACS) ' <<<"$hosting_output" || true
# contract_ids_on <view> <party-name>
contract_ids_on() {
  grep -E "^ACSID $1 party=$2 " <<<"$hosting_output" | awk '{print $4}' | sort
}
# print_acs_difference <party-name> <source-view>
print_acs_difference() {
  echo "$1 contracts on guardian4 and not on $2:"
  comm -13 <(contract_ids_on "$2" "$1") <(contract_ids_on guardian4 "$1") | sed 's/^/  /'
  echo "$1 contracts on $2 and not on guardian4:"
  comm -23 <(contract_ids_on "$2" "$1") <(contract_ids_on guardian4 "$1") | sed 's/^/  /'
}
for party_name in "${CHECKED_PARTIES[@]}"; do
  print_acs_difference "$party_name" "${REPLICATION_SOURCE_OF_PARTY[$party_name]}"
done

hosting_line_matches() {
  local view="$1" party_name="$2"
  grep -qE "^HOSTING $view party=$party_name serial=[0-9]+ threshold=2 hosts=$EXPECTED_HOSTS\$" <<<"$hosting_output"
}
# acs_digest_of <view> <party-name>
acs_digest_of() {
  grep -E "^ACS $1 party=$2 " <<<"$hosting_output" | sed -E 's/.*digest=([0-9a-f]{64})$/\1/'
}
# guardian4_acs_digest_equals_source <party-name> <source-view>
guardian4_acs_digest_equals_source() {
  local source_digest guardian4_digest
  source_digest="$(acs_digest_of "$2" "$1")"
  guardian4_digest="$(acs_digest_of guardian4 "$1")"
  [[ "$source_digest" =~ ^[0-9a-f]{64}$ && "$source_digest" == "$guardian4_digest" ]]
}
app_user_has_no_mapping() {
  ! grep -qE "^HOSTING [a-z0-9-]+ party=(guardianGovernance|feeCustody|nttCustody) .*app-user/" <<<"$hosting_output"
}

for view in "${CHECKED_VIEWS[@]}"; do
  for party_name in "${CHECKED_PARTIES[@]}"; do
    report_check_outcome "$view sees $party_name on $EXPECTED_HOSTS, threshold 2" hosting_line_matches "$view" "$party_name"
  done
done
report_check_outcome "no participant lists app-user as a host" app_user_has_no_mapping
for party_name in "${CHECKED_PARTIES[@]}"; do
  report_check_outcome "$party_name ACS on guardian4 equals ${REPLICATION_SOURCE_OF_PARTY[$party_name]} (contract IDs)" \
    guardian4_acs_digest_equals_source "$party_name" "${REPLICATION_SOURCE_OF_PARTY[$party_name]}"
done

if [[ "$check_failures" -ne 0 ]]; then
  fail "$check_failures check(s) failed"
fi
echo "PASS: gg_hosting_change_check"
