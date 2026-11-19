#!/usr/bin/env bash
# Offline replication of guardianGovernance (source app-provider) and nttCustody (source sv) to guardian4 at Confirmation.
# Usage: party_replication.sh <prepare|complete|remove-host|add-host-without-consent|set-threshold> [owner-name owner-name]
#   prepare        Steps 2 to 7: consent, disconnect guardian4, owner signatures, ACS export to $LOCALNET_STATE_DIR/guardian4/replication.
#   complete       Steps 10 to 13: ACS import with contract validation, reconnect, clear the onboarding flag.
#   remove-host    Remove REPLICATION_REMOVED_HOST (participant id or name) from both parties.
#   add-host-without-consent  Add REPLICATION_ADDED_HOST with the owner signatures only (negative check).
#   set-threshold  Set the confirmation threshold of both parties to REPLICATION_CONFIRMATION_THRESHOLD.
# Each PartyToParticipant change is compared with the intended change before any owner signs it.
# The owner names (default owner1 owner10) are two current guardianGovernance namespace owners under gg-keys/.
set -euo pipefail
# shellcheck source=gg_lib.sh
source "$(dirname "${BASH_SOURCE[0]}")/gg_lib.sh"

readonly KEY_TOOL="$REPO_ROOT/canton/devnet/guardian_key_tool.js"
readonly GG_KEY_DIR="$LOCALNET_STATE_DIR/gg-keys"
replication_dir="${REPLICATION_DIR:-$LOCALNET_STATE_DIR/guardian4/replication}"
replication_phase="${1:?usage: party_replication.sh <prepare|complete|remove-host|add-host-without-consent|set-threshold> [owner-name owner-name]}"
first_owner="${2:-owner1}"
second_owner="${3:-owner10}"
custody_party="$(jq -er .partyId "$LOCALNET_STATE_DIR/custody-artifacts.json")"

owner_signer_argv() { jq -cn --arg tool "$KEY_TOOL" --arg key "$GG_KEY_DIR/$1.key" '["node", $tool, "sign", $key]'; }

mkdir -p "$replication_dir"
LEDGER_API_TOKEN="$(cat "$gg_work_dir/script.jwt")" \
REPLICATION_PHASE="$replication_phase" REPLICATION_TARGET=guardian4 REPLICATION_DIR="$replication_dir" \
REPLICATION_PARTIES="$GG_PARTY=app-provider,$custody_party=sv" \
OWNER_PUBKEY_1="$GG_KEY_DIR/$first_owner.pub" OWNER_SIGN_ARGV_1="$(owner_signer_argv "$first_owner")" \
OWNER_PUBKEY_2="$GG_KEY_DIR/$second_owner.pub" OWNER_SIGN_ARGV_2="$(owner_signer_argv "$second_owner")" \
CANTON_CONSOLE_CONFIG="$LOCALNET_SCRIPT_DIR/console/remote-participants-guardian4.conf" \
  "$LOCALNET_SCRIPT_DIR/canton_console.sh" "$OWNER_SIGNING_PRELUDE" "$LOCALNET_SCRIPT_DIR/console/party_replication.canton" 2>&1 \
  | tee "$gg_work_dir/replication.log" | grep -E '^REPLICATION|ERROR|error|\^' || true
grep -q "^REPLICATION $replication_phase done" "$gg_work_dir/replication.log" || fail "party replication phase $replication_phase did not finish"
