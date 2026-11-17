#!/usr/bin/env bash
# Genesis on LocalNet: create the CoreState and the EmitterRegistry in one transaction, co-signed by Operator
# and guardianGovernance (2-of-3 owner signatures), then the ReplayRootRegistry (Operator only).
# A genesis with 1 of 3 owner signatures must fail first.
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

dso_party="$(curl -sSf "$(json_api_url sv)/v2/parties?pageSize=1000" -H "@$ADMIN_AUTH_HEADER" \
  | jq -er '[.partyDetails[].party | select(startswith("DSO::"))] | if length == 1 then .[0] else error("expected one DSO party") end')"

core_state_arguments="$(jq -n --arg operator "$OPERATOR_PARTY" --arg gg "$GG_PARTY" --arg dso "$dso_party" \
  --arg guardian "$DEVNET_GUARDIAN_ADDRESS" --arg governanceContract "$GOVERNANCE_CONTRACT" '{
    operator: $operator, guardianGovernance: $gg, chainId: "75", governanceChainId: "1",
    governanceContract: $governanceContract, guardianSetIndex: "0",
    guardianSets: [["0", {keys: [$guardian], expirationTime: null}]],
    messageFee: "0", feeInstrument: {admin: $dso, id: "Amulet"}, feeRecipient: $gg,
    consumedGovernance: {map: []}}')"
emitter_registry_arguments="$(jq -n --arg operator "$OPERATOR_PARTY" --arg gg "$GG_PARTY" \
  '{operator: $operator, guardianGovernance: $gg, registrationFee: "0", nextId: "0"}')"
genesis_commands="$(jq -c -n --arg coreStateTemplate "$CORE_STATE_TEMPLATE" --argjson coreStateArguments "$core_state_arguments" \
  --arg registryTemplate "$EMITTER_REGISTRY_TEMPLATE" --argjson registryArguments "$emitter_registry_arguments" \
  '[{CreateCommand: {templateId: $coreStateTemplate, createArguments: $coreStateArguments}},
    {CreateCommand: {templateId: $registryTemplate, createArguments: $registryArguments}}]')"
operator_and_gg="$(jq -n --arg op "$OPERATOR_PARTY" --arg gg "$GG_PARTY" '[$op, $gg]')"

echo "Negative check: genesis with Operator + guardianGovernance owner1 only (1 of 3)"
if submit_interactive_commands "genesis-one-owner-$(date +%s)" "$operator_and_gg" "$genesis_commands" \
    "$(jq -s 'add' <(operator_signers_json) <(gg_signers_json owner1))" \
    > "$gg_work_dir/genesis-one-owner.out" 2> "$gg_work_dir/genesis-one-owner.err"; then
  echo "FAIL: genesis committed with 1 of 3 guardianGovernance owner signatures" >&2
  exit 1
fi
for expected_fragment in FAILED_TO_EXECUTE_TRANSACTION "Received 1 valid signatures from distinct keys" \
    "but expected at least 2 valid for guardianGovernance::"; do
  if ! grep -qF "$expected_fragment" "$gg_work_dir/genesis-one-owner.err"; then
    echo "FAIL: the 1-of-3 genesis rejection does not contain: $expected_fragment" >&2
    cat "$gg_work_dir/genesis-one-owner.err" >&2
    exit 1
  fi
done
echo "PASS: rejected: FAILED_TO_EXECUTE_TRANSACTION, 1 valid of 2 required"

echo "Genesis: CoreState and EmitterRegistry with Operator + guardianGovernance owner1, owner2 (2-of-3)"
genesis_transaction="$(submit_interactive_commands "genesis-core-$(date +%s)" "$operator_and_gg" "$genesis_commands" \
  "$(jq -s 'add' <(operator_signers_json) <(gg_signers_json owner1 owner2))")"
core_state_cid="$(created_contract_id "$genesis_transaction" ":Wormhole.Core.State:CoreState")"
record_core_contract_id coreState "$core_state_cid"
record_core_contract_id emitterRegistry "$(created_contract_id "$genesis_transaction" ":Wormhole.Core.State:EmitterRegistry")"
echo "CoreState $core_state_cid (update $(jq -r .updateId <<<"$genesis_transaction"))"

operator_only="$(jq -n --arg op "$OPERATOR_PARTY" '[$op]')"
replay_registry_arguments="$(jq -n --arg operator "$OPERATOR_PARTY" --arg gg "$GG_PARTY" \
  '{operator: $operator, guardianGovernance: $gg, claimFee: "0"}')"
replay_transaction="$(submit_interactive "genesis-replay-root-registry-$(date +%s)" "$operator_only" \
  "$(jq -n --arg templateId "$REPLAY_ROOT_REGISTRY_TEMPLATE" --argjson createArguments "$replay_registry_arguments" \
    '{CreateCommand: {templateId: $templateId, createArguments: $createArguments}}')" "$(operator_signers_json)")"
record_core_contract_id replayRootRegistry "$(created_contract_id "$replay_transaction" ":Wormhole.Core.State:ReplayRootRegistry")"

jq . "$CORE_CONTRACTS"
