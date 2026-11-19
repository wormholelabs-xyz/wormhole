#!/usr/bin/env bash
# Genesis on LocalNet: create the CoreState, then the EmitterRegistry, each co-signed by Operator and
# guardianGovernance (2-of-3 owner signatures), then the ReplayRootRegistry (Operator only).
# The prepare endpoint takes one command, so each create is its own transaction.
# A CoreState create with 1 of 3 owner signatures must fail first.
# Rerun after a failure: the script creates only the missing contracts.
# Usage: [GENESIS_GUARDIAN_SET_INDEX=<index>] gg_genesis.sh (default index 0)
# Then create the user wormhole-watcher with only CanReadAs(guardianGovernance) on each guardian host, for the
# emitter id scan.
# Precondition: gg_host.sh ran. Output: $LOCALNET_STATE_DIR/core-contracts.json.
set -euo pipefail
# shellcheck source=gg_lib.sh
source "$(dirname "${BASH_SOURCE[0]}")/gg_lib.sh"
# shellcheck source=watcher_harness_lib.sh
source "$LOCALNET_SCRIPT_DIR/watcher_harness_lib.sh"

readonly DEVNET_GUARDIAN_ADDRESS="befa429d57cd18b7f8a4d91a2da9ab4af05d0fbe"
readonly GOVERNANCE_CONTRACT="0000000000000000000000000000000000000000000000000000000000000004"
# The global guardian set index of the genesis set; the CoreState precondition requires 0 to 4294967295.
readonly GENESIS_GUARDIAN_SET_INDEX="${GENESIS_GUARDIAN_SET_INDEX:-0}"
if [[ ! "$GENESIS_GUARDIAN_SET_INDEX" =~ ^(0|[1-9][0-9]{0,9})$ ]] || (( GENESIS_GUARDIAN_SET_INDEX > 4294967295 )); then
  echo "GENESIS_GUARDIAN_SET_INDEX must be a decimal integer from 0 to 4294967295, got: $GENESIS_GUARDIAN_SET_INDEX" >&2
  exit 1
fi

# active_operator_contract_ids <qualified-template-suffix>: JSON array of the active contract ids of that
# template with operator = Operator, from the app-provider ACS.
active_operator_contract_ids() {
  active_contracts "$SUBMITTING_VALIDATOR" "$OPERATOR_PARTY" "$1" \
    | jq -c --arg operator "$OPERATOR_PARTY" '[.[] | select(.createArgument.operator == $operator) | .contractId]'
}

# create_command <template-id> <create-arguments-json>
create_command() {
  jq -n --arg templateId "$1" --argjson createArguments "$2" \
    '{CreateCommand: {templateId: $templateId, createArguments: $createArguments}}'
}

active_core_state_ids="$(active_operator_contract_ids :Wormhole.Core.State:CoreState)"
active_emitter_registry_ids="$(active_operator_contract_ids :Wormhole.Core.State:EmitterRegistry)"
active_replay_root_registry_ids="$(active_operator_contract_ids :Wormhole.Core.State:ReplayRootRegistry)"
active_core_state_count="$(jq length <<<"$active_core_state_ids")"
active_emitter_registry_count="$(jq length <<<"$active_emitter_registry_ids")"
active_replay_root_registry_count="$(jq length <<<"$active_replay_root_registry_ids")"
operator_and_gg="$(jq -n --arg op "$OPERATOR_PARTY" --arg gg "$GG_PARTY" '[$op, $gg]')"
co_signers="$(jq -s 'add' <(operator_signers_json) <(gg_signers_json owner1 owner2))"

case "$active_core_state_count" in
  0)
    if [[ "$active_emitter_registry_count" != 0 ]]; then
      echo "found $active_emitter_registry_count EmitterRegistry and no CoreState; the CoreState is created first" >&2
      exit 1
    fi
    dso_party="$(curl -sSf "$(json_api_url sv)/v2/parties?pageSize=1000" -H "@$ADMIN_AUTH_HEADER" \
      | jq -er '[.partyDetails[].party | select(startswith("DSO::"))] | if length == 1 then .[0] else error("expected one DSO party") end')"
    core_state_arguments="$(jq -n --arg operator "$OPERATOR_PARTY" --arg gg "$GG_PARTY" --arg dso "$dso_party" \
      --arg guardian "$DEVNET_GUARDIAN_ADDRESS" --arg governanceContract "$GOVERNANCE_CONTRACT" \
      --arg guardianSetIndex "$GENESIS_GUARDIAN_SET_INDEX" '{
        operator: $operator, guardianGovernance: $gg, chainId: "75", governanceChainId: "1",
        governanceContract: $governanceContract, guardianSetIndex: $guardianSetIndex,
        guardianSets: [[$guardianSetIndex, {keys: [$guardian], expirationTime: null}]],
        messageFee: "0", feeInstrument: {admin: $dso, id: "Amulet"}, feeRecipient: $gg,
        consumedGovernance: {map: []}}')"
    create_core_state="$(create_command "$CORE_STATE_TEMPLATE" "$core_state_arguments")"

    echo "Negative check: CoreState with Operator + guardianGovernance owner1 only (1 of 3)"
    if submit_interactive "genesis-core-state-one-owner-$(date +%s)" "$operator_and_gg" "$create_core_state" \
        "$(jq -s 'add' <(operator_signers_json) <(gg_signers_json owner1))" \
        > "$gg_work_dir/genesis-one-owner.out" 2> "$gg_work_dir/genesis-one-owner.err"; then
      echo "FAIL: CoreState committed with 1 of 3 guardianGovernance owner signatures" >&2
      exit 1
    fi
    for expected_fragment in FAILED_TO_EXECUTE_TRANSACTION \
        "Received 1 valid signatures from distinct keys (0 invalid), but expected at least 2 valid for guardianGovernance::"; do
      if ! grep -qF "$expected_fragment" "$gg_work_dir/genesis-one-owner.err"; then
        echo "FAIL: the 1-of-3 genesis rejection does not contain: $expected_fragment" >&2
        cat "$gg_work_dir/genesis-one-owner.err" >&2
        exit 1
      fi
    done
    echo "PASS: rejected: FAILED_TO_EXECUTE_TRANSACTION, 1 valid of 2 required"

    echo "Genesis: CoreState with Operator + guardianGovernance owner1, owner2 (2-of-3)"
    core_state_transaction="$(submit_interactive "genesis-core-state-$(date +%s)" "$operator_and_gg" \
      "$create_core_state" "$co_signers")"
    echo "CoreState $(created_contract_id "$core_state_transaction" ":Wormhole.Core.State:CoreState") (update $(jq -r .updateId <<<"$core_state_transaction"))"
    ;;
  1)
    echo "CoreState exists; skip"
    ;;
  *)
    echo "expected 0 or 1 CoreState, found $active_core_state_count" >&2
    exit 1
    ;;
esac

case "$active_emitter_registry_count" in
  0)
    echo "Genesis: EmitterRegistry with Operator + guardianGovernance owner1, owner2 (2-of-3)"
    emitter_registry_arguments="$(jq -n --arg operator "$OPERATOR_PARTY" --arg gg "$GG_PARTY" \
      '{operator: $operator, guardianGovernance: $gg, registrationFee: "0", nextId: "0"}')"
    registry_transaction="$(submit_interactive "genesis-emitter-registry-$(date +%s)" "$operator_and_gg" \
      "$(create_command "$EMITTER_REGISTRY_TEMPLATE" "$emitter_registry_arguments")" "$co_signers")"
    record_core_contract_id emitterRegistry "$(created_contract_id "$registry_transaction" ":Wormhole.Core.State:EmitterRegistry")"
    ;;
  1)
    echo "EmitterRegistry exists; skip"
    record_core_contract_id emitterRegistry "$(jq -r '.[0]' <<<"$active_emitter_registry_ids")"
    ;;
  *)
    echo "expected 0 or 1 EmitterRegistry, found $active_emitter_registry_count" >&2
    exit 1
    ;;
esac

case "$active_replay_root_registry_count" in
  0)
    operator_only="$(jq -n --arg op "$OPERATOR_PARTY" '[$op]')"
    replay_registry_arguments="$(jq -n --arg operator "$OPERATOR_PARTY" --arg gg "$GG_PARTY" \
      '{operator: $operator, guardianGovernance: $gg, claimFee: "0"}')"
    replay_transaction="$(submit_interactive "genesis-replay-root-registry-$(date +%s)" "$operator_only" \
      "$(create_command "$REPLAY_ROOT_REGISTRY_TEMPLATE" "$replay_registry_arguments")" "$(operator_signers_json)")"
    record_core_contract_id replayRootRegistry "$(created_contract_id "$replay_transaction" ":Wormhole.Core.State:ReplayRootRegistry")"
    ;;
  1)
    echo "ReplayRootRegistry exists; skip"
    record_core_contract_id replayRootRegistry "$(jq -r '.[0]' <<<"$active_replay_root_registry_ids")"
    ;;
  *)
    echo "expected 0 or 1 ReplayRootRegistry, found $active_replay_root_registry_count" >&2
    exit 1
    ;;
esac

jq . "$CORE_CONTRACTS"

for validator_name in "${GUARDIAN_HOSTS[@]}"; do
  ensure_watcher_user "$validator_name"
done
