#!/usr/bin/env bash
# Check the end state of the guardian replacement rehearsal (L1): guardianGovernance and nttCustody are hosted on
# sv, app-provider and guardian4 at Confirmation with threshold 2, as each participant sees it, and the guardianGovernance
# ACS on guardian4 equals the ACS on app-provider.
# Usage: gg_hosting_change_check.sh
set -euo pipefail
# shellcheck source=gg_lib.sh
source "$(dirname "${BASH_SOURCE[0]}")/gg_lib.sh"

readonly EXPECTED_HOSTS="app-provider/Confirmation,guardian4/Confirmation,sv/Confirmation"
readonly CHECKED_VIEWS=(sv app-provider guardian4)
custody_party="$(jq -er .partyId "$LOCALNET_STATE_DIR/custody-artifacts.json")"
check_failures=0

check_that() {
  local description="$1"
  shift
  if "$@"; then
    echo "PASS: $description"
  else
    echo "FAIL: $description"
    check_failures=$((check_failures + 1))
  fi
}

hosting_output="$(LEDGER_API_TOKEN="$("$LOCALNET_SCRIPT_DIR/token.sh" "$SCRIPT_USER_ID")" HOSTING_PARTY_IDS="$GG_PARTY,$custody_party" HOSTING_ACS_DIGEST=true \
  CANTON_CONSOLE_CONFIG="$LOCALNET_SCRIPT_DIR/console/remote-participants-guardian4.conf" \
  "$LOCALNET_SCRIPT_DIR/canton_console.sh" "$LOCALNET_SCRIPT_DIR/console/hosting_topology.canton" 2>&1)" || true
grep -E '^(HOSTING|ACS) ' <<<"$hosting_output" || true
contract_ids_on() {
  grep -E "^ACSID $1 party=guardianGovernance " <<<"$hosting_output" | awk '{print $4}' | sort
}
echo "guardianGovernance contracts on guardian4 and not on app-provider:"
comm -13 <(contract_ids_on app-provider) <(contract_ids_on guardian4) | sed 's/^/  /'
echo "guardianGovernance contracts on app-provider and not on guardian4:"
comm -23 <(contract_ids_on app-provider) <(contract_ids_on guardian4) | sed 's/^/  /'

hosting_line_matches() {
  local view="$1" party_name="$2"
  grep -qE "^HOSTING $view party=$party_name serial=[0-9]+ threshold=2 hosts=$EXPECTED_HOSTS\$" <<<"$hosting_output"
}
acs_digest_of() {
  grep -E "^ACS $1 party=guardianGovernance " <<<"$hosting_output" | sed -E 's/.*digest=([0-9a-f]{64})$/\1/'
}
acs_digests_are_equal() {
  local provider_digest guardian4_digest
  provider_digest="$(acs_digest_of app-provider)"
  guardian4_digest="$(acs_digest_of guardian4)"
  [[ "$provider_digest" =~ ^[0-9a-f]{64}$ && "$provider_digest" == "$guardian4_digest" ]]
}
app_user_has_no_mapping() {
  ! grep -qE "^HOSTING [a-z0-9-]+ party=(guardianGovernance|nttCustody) .*app-user/" <<<"$hosting_output"
}

for view in "${CHECKED_VIEWS[@]}"; do
  check_that "$view sees guardianGovernance on $EXPECTED_HOSTS, threshold 2" hosting_line_matches "$view" guardianGovernance
  check_that "$view sees nttCustody on $EXPECTED_HOSTS, threshold 2" hosting_line_matches "$view" nttCustody
done
check_that "no participant lists app-user as a host" app_user_has_no_mapping
check_that "guardianGovernance ACS on guardian4 equals app-provider (contract IDs)" acs_digests_are_equal

if [[ "$check_failures" -ne 0 ]]; then
  fail "$check_failures check(s) failed"
fi
echo "PASS: gg_hosting_change_check"
