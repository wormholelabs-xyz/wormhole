#!/usr/bin/env bash
# L5 snapshot forgery (LocalNet only): run console/forge_coin_contract.canton with a Ledger API token for app-provider.
# Usage: forge_coin_contract.sh            generate FORGE_COIN_COUNT invented coins into FORGE_OUTPUT_FILE
#        FORGE_MODE=merge FORGE_MERGE_BASE=<export.gz> FORGE_MERGE_OUTPUT=<out.gz> forge_coin_contract.sh
set -euo pipefail
# shellcheck source=env.sh
source "$(dirname "${BASH_SOURCE[0]}")/env.sh"

forge_work_dir="$(mktemp -d)"
trap 'rm -rf "$forge_work_dir"' EXIT
# The console reads app-provider's ACS as gg; wormhole-script has CanReadAsAnyParty.
script_user_token="$("$LOCALNET_SCRIPT_DIR/token.sh" "$SCRIPT_USER_ID")"
(umask 077 && sed "s|^    app-provider {|    app-provider {\n      token = \"$script_user_token\"|" \
  "$LOCALNET_SCRIPT_DIR/console/remote-participants.conf" > "$forge_work_dir/console.conf")

GG_PARTY_ID="$(jq -er .partyId "$LOCALNET_STATE_DIR/gg-artifacts.json")" \
NTT_MANAGER_CID="${NTT_MANAGER_CID:-$(jq -er .burnMint.managerCid "$LOCALNET_STATE_DIR/ntt-contracts.json")}" \
FORGE_DAR_DIR="${FORGE_DAR_DIR:-$LOCALNET_STATE_DIR/guardian4/dars}" \
FORGE_OUTPUT_FILE="${FORGE_OUTPUT_FILE:-$LOCALNET_STATE_DIR/forge/forged-coins.acs.gz}" \
CANTON_CONSOLE_CONFIG="$forge_work_dir/console.conf" \
  "$LOCALNET_SCRIPT_DIR/canton_console.sh" "$LOCALNET_SCRIPT_DIR/console/owner_signing_prelude.canton" \
  "$LOCALNET_SCRIPT_DIR/console/forge_coin_contract.canton" 2>&1 \
  | grep -E '^FORGE|ERROR'
