#!/usr/bin/env bash
# Genesis on LocalNet: create the CoreState co-signed by Operator and guardianGovernance (2-of-3 owner
# signatures), then the EmitterRegistry and the ReplayRootRegistry (Operator only).
# Usage: gg_genesis.sh
# Precondition: gg_host.sh ran. Output: $LOCALNET_STATE_DIR/core-contracts.json.
set -euo pipefail
# shellcheck source=gg_lib.sh
source "$(dirname "${BASH_SOURCE[0]}")/gg_lib.sh"

readonly DEVNET_GUARDIAN_ADDRESS="befa429d57cd18b7f8a4d91a2da9ab4af05d0fbe"
readonly GOVERNANCE_CONTRACT="0000000000000000000000000000000000000000000000000000000000000004"

if [[ -f "$CORE_CONTRACTS" ]]; then
  echo "$CORE_CONTRACTS exists. Genesis ran already; CoreState is a singleton." >&2
  exit 1
fi

admin_token="$("$LOCALNET_SCRIPT_DIR/token.sh")"
dso_party="$(curl -sSf "$(json_api_url sv)/v2/parties?pageSize=1000" -H "Authorization: Bearer $admin_token" \
  | jq -er '[.partyDetails[].party | select(startswith("DSO::"))] | if length == 1 then .[0] else error("expected one DSO party") end')"

core_state_arguments="$(jq -n --arg operator "$OPERATOR_PARTY" --arg gg "$GG_PARTY" --arg dso "$dso_party" \
  --arg guardian "$DEVNET_GUARDIAN_ADDRESS" --arg governanceContract "$GOVERNANCE_CONTRACT" '{
    operator: $operator, guardianGovernance: $gg, chainId: "75", governanceChainId: "1",
    governanceContract: $governanceContract, guardianSetIndex: "0",
    guardianSets: [["0", {keys: [$guardian], expirationTime: null}]],
    messageFee: "0", feeInstrument: {admin: $dso, id: "Amulet"}, feeRecipient: $gg,
    consumedGovernance: {map: []}}')"
create_core_state="$(jq -n --arg templateId "$CORE_STATE_TEMPLATE" --argjson createArguments "$core_state_arguments" \
  '{CreateCommand: {templateId: $templateId, createArguments: $createArguments}}')"
genesis_signers="$(jq -s 'add' <(operator_signers_json) <(gg_signers_json owner1 owner2))"

echo "Genesis: CoreState with Operator + guardianGovernance owner1, owner2 (2-of-3)"
genesis_transaction="$(submit_interactive "genesis-core-state-$(date +%s)" \
  "$(jq -n --arg op "$OPERATOR_PARTY" --arg gg "$GG_PARTY" '[$op, $gg]')" "$create_core_state" "$genesis_signers")"
core_state_cid="$(created_contract_id "$genesis_transaction" ":Wormhole.Core.State:CoreState")"
record_core_contract_id coreState "$core_state_cid"
echo "CoreState $core_state_cid (update $(jq -r .updateId <<<"$genesis_transaction"))"

operator_only='[]'
operator_only="$(jq -n --arg op "$OPERATOR_PARTY" '[$op]')"
emitter_registry_arguments="$(jq -n --arg operator "$OPERATOR_PARTY" --arg gg "$GG_PARTY" \
  '{operator: $operator, guardianGovernance: $gg, registrationFee: "0", nextId: "0"}')"
registry_transaction="$(submit_interactive "genesis-emitter-registry-$(date +%s)" "$operator_only" \
  "$(jq -n --arg templateId "$EMITTER_REGISTRY_TEMPLATE" --argjson createArguments "$emitter_registry_arguments" \
    '{CreateCommand: {templateId: $templateId, createArguments: $createArguments}}')" "$(operator_signers_json)")"
record_core_contract_id emitterRegistry "$(created_contract_id "$registry_transaction" ":Wormhole.Core.State:EmitterRegistry")"

replay_registry_arguments="$(jq -n --arg operator "$OPERATOR_PARTY" --arg gg "$GG_PARTY" \
  '{operator: $operator, guardianGovernance: $gg, claimFee: "0"}')"
replay_transaction="$(submit_interactive "genesis-replay-root-registry-$(date +%s)" "$operator_only" \
  "$(jq -n --arg templateId "$REPLAY_ROOT_REGISTRY_TEMPLATE" --argjson createArguments "$replay_registry_arguments" \
    '{CreateCommand: {templateId: $templateId, createArguments: $createArguments}}')" "$(operator_signers_json)")"
record_core_contract_id replayRootRegistry "$(created_contract_id "$replay_transaction" ":Wormhole.Core.State:ReplayRootRegistry")"

jq . "$CORE_CONTRACTS"
