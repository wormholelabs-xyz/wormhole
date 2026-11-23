#!/usr/bin/env bash
# Offline replication of guardianGovernance and feeCustody (source app-provider) and nttCustody (source sv) to guardian4
# at Confirmation. Each phase changes the three parties together: they keep the same hosting.
# Usage: party_replication.sh <phase> [owner-name owner-name]
#   connect        Connect guardian4 to the global synchronizer and vet every DAR that app-provider has.
#   prepare        Steps 2 to 7: consent, disconnect guardian4, owner signatures, ACS export to $LOCALNET_STATE_DIR/guardian4/replication.
#   complete       Steps 10 to 13: ACS import with contract validation, reconnect, clear the onboarding flag.
#   remove-host    Remove REPLICATION_REMOVED_HOST (participant id or name) from each party.
#   add-host-without-consent  Add REPLICATION_ADDED_HOST with the owner signatures only (negative check).
#   set-threshold  Set the confirmation threshold of each party to REPLICATION_CONFIRMATION_THRESHOLD.
#   set-permission Set REPLICATION_PERMISSION_HOST (console name) to REPLICATION_HOST_PERMISSION (Observation or
#                  Confirmation) on each party. An increase needs the consent of that host.
#   set-permission-without-consent  Increase that permission with the owner signatures only (negative check).
# Each PartyToParticipant change is compared with the intended change before any owner signs it.
# The owner names (default owner1 owner2) are two current guardianGovernance namespace owners under gg-keys/.
set -euo pipefail
# shellcheck source=gg_lib.sh
source "$(dirname "${BASH_SOURCE[0]}")/gg_lib.sh"

readonly KEY_TOOL="$REPO_ROOT/canton/devnet/guardian_key_tool.js"
readonly GG_KEY_DIR="$LOCALNET_STATE_DIR/gg-keys"
replication_dir="${REPLICATION_DIR:-$LOCALNET_STATE_DIR/guardian4/replication}"
replication_phase="${1:?usage: party_replication.sh <connect|prepare|complete|remove-host|add-host-without-consent|set-threshold|set-permission|set-permission-without-consent> [owner-name owner-name]}"
first_owner="${2:-owner1}"
second_owner="${3:-owner2}"
check_owner_names_are_safe "$first_owner" "$second_owner"
check_owner_names_differ "$first_owner" "$second_owner"
[[ -n "$FEE_CUSTODY_PARTY" ]] || fail "missing $FEE_CUSTODY_ARTIFACTS. Run CUSTODY_PARTY_NAME=feeCustody custody_host.sh first."
[[ -n "$CUSTODY_PARTY" ]] || fail "missing $CUSTODY_ARTIFACTS. Run CUSTODY_PARTY_NAME=nttCustody custody_host.sh first."

owner_signer_argv() { jq -cn --arg tool "$KEY_TOOL" --arg key "$GG_KEY_DIR/$1.key" '["node", $tool, "sign", $key]'; }

if [[ "$replication_phase" == connect ]]; then
  guardian4_dar_dir="$(mktemp -d "$gg_work_dir/guardian4-dars-XXXXXX")"
  # The admin subject (ledger-api-user) can create the Daml Script user on the fresh guardian4 ledger API.
  LEDGER_API_TOKEN="$("$LOCALNET_SCRIPT_DIR/token.sh")" \
  GUARDIAN4_DAR_DIR="$guardian4_dar_dir" \
  CANTON_CONSOLE_CONFIG="$LOCALNET_SCRIPT_DIR/console/remote-participants-guardian4.conf" \
    "$LOCALNET_SCRIPT_DIR/canton_console.sh" "$LOCALNET_SCRIPT_DIR/console/guardian4_connect.canton" 2>&1 \
    | tee "$gg_work_dir/connect.log" | grep -E '^GUARDIAN4|ERROR|error|\^' || true
  grep -qE "^GUARDIAN4 participant=.* connected=true" "$gg_work_dir/connect.log" || fail "guardian4 did not connect"
  exit 0
fi

mkdir -p "$replication_dir"
LEDGER_API_TOKEN="$(cat "$gg_work_dir/script.jwt")" \
REPLICATION_PHASE="$replication_phase" REPLICATION_TARGET=guardian4 REPLICATION_DIR="$replication_dir" \
REPLICATION_PARTIES="$GG_PARTY=app-provider,$FEE_CUSTODY_PARTY=app-provider,$CUSTODY_PARTY=sv" \
OWNER_PUBKEY_1="$GG_KEY_DIR/$first_owner.pub" OWNER_SIGN_ARGV_1="$(owner_signer_argv "$first_owner")" \
OWNER_PUBKEY_2="$GG_KEY_DIR/$second_owner.pub" OWNER_SIGN_ARGV_2="$(owner_signer_argv "$second_owner")" \
CANTON_CONSOLE_CONFIG="$LOCALNET_SCRIPT_DIR/console/remote-participants-guardian4.conf" \
  "$LOCALNET_SCRIPT_DIR/canton_console.sh" "$OWNER_SIGNING_PRELUDE" "$LOCALNET_SCRIPT_DIR/console/owner_key_rotation_lib.canton" \
    "$LOCALNET_SCRIPT_DIR/console/party_replication_lib.canton" "$LOCALNET_SCRIPT_DIR/console/party_replication.canton" 2>&1 \
  | tee "$gg_work_dir/replication.log" | grep -E '^REPLICATION|ERROR|error|\^' || true
grep -q "^REPLICATION $replication_phase done" "$gg_work_dir/replication.log" || fail "party replication phase $replication_phase did not finish"
