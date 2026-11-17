#!/usr/bin/env bash
# Allocate nttCustody under the guardianGovernance decentralized namespace (console/custody_allocate_party.canton).
# nttCustody gets the hosts, the confirmation threshold and the 2-of-3 signing keys of guardianGovernance.
# Then show both parties' topology on each participant.
# Usage: custody_host.sh <owner> <owner> <owner>
#   The three current owner names: key file prefixes under $LOCALNET_STATE_DIR/gg-keys, for example: owner1 owner2 owner3
#   CUSTODY_PARTY_NAME overrides the party name (default nttCustody).
# Output: $LOCALNET_STATE_DIR/custody-artifacts.json
# Precondition: gg_host.sh ran.
set -euo pipefail
# shellcheck source=gg_lib.sh
source "$(dirname "${BASH_SOURCE[0]}")/gg_lib.sh"

readonly KEY_TOOL="$REPO_ROOT/canton/devnet/guardian_key_tool.js"
readonly GG_KEY_DIR="$LOCALNET_STATE_DIR/gg-keys"
export CUSTODY_PARTY_NAME="${CUSTODY_PARTY_NAME:-nttCustody}"

if [[ $# -ne 3 ]]; then
  echo "usage: custody_host.sh <owner> <owner> <owner>" >&2
  exit 2
fi
if [[ ! "$CUSTODY_PARTY_NAME" =~ ^[A-Za-z][A-Za-z0-9_-]*$ ]]; then
  echo "invalid party name: $CUSTODY_PARTY_NAME" >&2
  exit 2
fi
owner_slot=0
for owner_name in "$@"; do
  owner_slot=$((owner_slot + 1))
  for key_file in "$GG_KEY_DIR/$owner_name.pub" "$GG_KEY_DIR/$owner_name.key"; do
    if [[ ! -f "$key_file" ]]; then
      echo "Missing $key_file" >&2
      exit 1
    fi
  done
  export "CUSTODY_OWNER_PUBKEY_$owner_slot=$GG_KEY_DIR/$owner_name.pub"
  export "CUSTODY_OWNER_SIGN_ARGV_$owner_slot=$(jq -cn --arg tool "$KEY_TOOL" --arg key "$GG_KEY_DIR/$owner_name.key" '["node", $tool, "sign", $key]')"
done

allocation_status=0
GG_PARTY_ID="$GG_PARTY" CUSTODY_ARTIFACTS_FILE="$CUSTODY_ARTIFACTS" \
  "$LOCALNET_SCRIPT_DIR/canton_console.sh" "$OWNER_SIGNING_PRELUDE" "$LOCALNET_SCRIPT_DIR/console/custody_allocate_party.canton" \
  > "$gg_work_dir/allocation.log" 2>&1 || allocation_status=$?
grep -E '^CUSTODY' "$gg_work_dir/allocation.log" || true
if [[ "$allocation_status" -ne 0 ]] || ! grep -q '^CUSTODY done' "$gg_work_dir/allocation.log"; then
  echo "FAIL: custody_allocate_party.canton did not finish (status $allocation_status)" >&2
  grep -E 'ERROR|error|Exception' "$gg_work_dir/allocation.log" | head -n 20 >&2
  exit 1
fi
custody_party_id="$(jq -er .partyId "$CUSTODY_ARTIFACTS")"
if [[ "${custody_party_id#*::}" != "${GG_PARTY#*::}" ]]; then
  echo "FAIL: $custody_party_id is outside the namespace of $GG_PARTY" >&2
  exit 1
fi
echo "PASS: $custody_party_id shares the namespace of $GG_PARTY"
"$LOCALNET_SCRIPT_DIR/gg_show_topology.sh"
