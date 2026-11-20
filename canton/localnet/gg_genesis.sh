#!/usr/bin/env bash
# Genesis on LocalNet: create the CoreState, co-signed by Operator, guardianGovernance and feeCustody (2-of-3 owner
# signatures each), then the EmitterRegistry, co-signed by Operator and guardianGovernance, then the
# ReplayRootRegistry (Operator only). feeRecipient is feeCustody.
# The prepare endpoint takes one command, so each create is its own transaction.
# Before the first prepare: feeCustody is in the guardianGovernance namespace and is not nttCustody.
# Negative checks before the CoreState commit, each must fail:
#   guardianGovernance with 1 of 3 owner signatures; no feeCustody signatures; feeCustody with 1 of 3 owner
#   signatures; feeRecipient = guardianGovernance and fee instrument admin = guardianGovernance (CoreState ensure).
# Rerun after a failure: the script creates only the missing contracts. It checks the feeRecipient and the
# signatories of an active CoreState.
# Usage: [GENESIS_GUARDIAN_SET_INDEX=<index>] gg_genesis.sh (default index 0)
# Then create the user wormhole-watcher with only CanReadAs(guardianGovernance) on each guardian host, for the
# emitter id scan.
# Precondition: gg_host.sh ran, and custody_host.sh ran with CUSTODY_PARTY_NAME=feeCustody.
# Output: $LOCALNET_STATE_DIR/core-contracts.json.
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

if [[ -z "$FEE_CUSTODY_PARTY" ]]; then
  fail "missing $FEE_CUSTODY_ARTIFACTS. Run CUSTODY_PARTY_NAME=feeCustody custody_host.sh first."
fi
check_fee_custody_is_in_the_guardian_namespace "$FEE_CUSTODY_PARTY" "$GG_PARTY" "$CUSTODY_PARTY" || exit 1

# expect_genesis_rejected <check-name> <create-command-json> <signers-json> <expected-text>...: the CoreState create
# fails with each expected text.
expect_genesis_rejected() {
  local check_name="$1" create_command_json="$2" signers_json="$3" expected_fragment
  shift 3
  echo "Negative check: $check_name"
  if submit_interactive "genesis-core-state-$check_name-$(date +%s)" "$operator_gg_and_fee_custody" "$create_command_json" \
      "$signers_json" > "$gg_work_dir/genesis-$check_name.out" 2> "$gg_work_dir/genesis-$check_name.err"; then
    fail "CoreState committed: $check_name"
  fi
  for expected_fragment in "$@"; do
    if ! grep -qF -- "$expected_fragment" "$gg_work_dir/genesis-$check_name.err"; then
      echo "FAIL: the rejection of $check_name does not contain: $expected_fragment" >&2
      cat "$gg_work_dir/genesis-$check_name.err" >&2
      exit 1
    fi
  done
  echo "PASS: rejected: $*"
}

active_core_state_ids="$(active_operator_contract_ids :Wormhole.Core.State:CoreState)"
active_emitter_registry_ids="$(active_operator_contract_ids :Wormhole.Core.State:EmitterRegistry)"
active_replay_root_registry_ids="$(active_operator_contract_ids :Wormhole.Core.State:ReplayRootRegistry)"
active_core_state_count="$(jq length <<<"$active_core_state_ids")"
active_emitter_registry_count="$(jq length <<<"$active_emitter_registry_ids")"
active_replay_root_registry_count="$(jq length <<<"$active_replay_root_registry_ids")"
operator_and_gg="$(jq -n --arg op "$OPERATOR_PARTY" --arg gg "$GG_PARTY" '[$op, $gg]')"
operator_gg_and_fee_custody="$(jq -n --arg op "$OPERATOR_PARTY" --arg gg "$GG_PARTY" --arg feeCustody "$FEE_CUSTODY_PARTY" \
  '[$op, $gg, $feeCustody]')"
co_signers="$(jq -s 'add' <(operator_signers_json) <(gg_signers_json owner1 owner2))"
core_state_signers="$(jq -s 'add' <(operator_signers_json) <(gg_signers_json owner1 owner2) \
  <(namespace_party_signers_json "$FEE_CUSTODY_PARTY" owner1 owner2))"

case "$active_core_state_count" in
  0)
    if [[ "$active_emitter_registry_count" != 0 ]]; then
      echo "found $active_emitter_registry_count EmitterRegistry and no CoreState; the CoreState is created first" >&2
      exit 1
    fi
    dso_party="$(curl -sSf "$(json_api_url sv)/v2/parties?pageSize=1000" -H "@$ADMIN_AUTH_HEADER" \
      | jq -er '[.partyDetails[].party | select(startswith("DSO::"))] | if length == 1 then .[0] else error("expected one DSO party") end')"
    # core_state_arguments <fee-recipient-party-id> <fee-instrument-admin-party-id>
    core_state_arguments() {
      jq -n --arg operator "$OPERATOR_PARTY" --arg gg "$GG_PARTY" --arg feeInstrumentAdmin "$2" --arg feeRecipient "$1" \
        --arg guardian "$DEVNET_GUARDIAN_ADDRESS" --arg governanceContract "$GOVERNANCE_CONTRACT" \
        --arg guardianSetIndex "$GENESIS_GUARDIAN_SET_INDEX" '{
          operator: $operator, guardianGovernance: $gg, chainId: "75", governanceChainId: "1",
          governanceContract: {tag: "EmitterAddress", value: $governanceContract}, guardianSetIndex: $guardianSetIndex,
          guardianSets: [[$guardianSetIndex, {keys: [{tag: "GuardianAddress", value: $guardian}], expirationTime: null}]],
          messageFee: "0", feeInstrument: {admin: $feeInstrumentAdmin, id: "Amulet"}, feeRecipient: $feeRecipient,
          consumedGovernance: [], evictedGovernanceMaxTimestampSeconds: null}'
    }
    create_core_state="$(create_command "$CORE_STATE_TEMPLATE" "$(core_state_arguments "$FEE_CUSTODY_PARTY" "$dso_party")")"

    expect_genesis_rejected gg-one-owner "$create_core_state" \
      "$(jq -s 'add' <(operator_signers_json) <(gg_signers_json owner1) <(namespace_party_signers_json "$FEE_CUSTODY_PARTY" owner1 owner2))" \
      FAILED_TO_EXECUTE_TRANSACTION \
      "Received 1 valid signatures from distinct keys (0 invalid), but expected at least 2 valid for guardianGovernance::"
    expect_genesis_rejected no-fee-custody-signatures "$create_core_state" "$co_signers" \
      "did not provide an external signature: $FEE_CUSTODY_PARTY"
    expect_genesis_rejected fee-custody-one-owner "$create_core_state" \
      "$(jq -s 'add' <(operator_signers_json) <(gg_signers_json owner1 owner2) <(namespace_party_signers_json "$FEE_CUSTODY_PARTY" owner1))" \
      FAILED_TO_EXECUTE_TRANSACTION \
      "Received 1 valid signatures from distinct keys (0 invalid), but expected at least 2 valid for feeCustody::"
    expect_genesis_rejected fee-recipient-is-guardian-governance \
      "$(create_command "$CORE_STATE_TEMPLATE" "$(core_state_arguments "$GG_PARTY" "$dso_party")")" "$core_state_signers" \
      PreconditionFailed
    expect_genesis_rejected fee-instrument-admin-is-guardian-governance \
      "$(create_command "$CORE_STATE_TEMPLATE" "$(core_state_arguments "$FEE_CUSTODY_PARTY" "$GG_PARTY")")" "$core_state_signers" \
      PreconditionFailed

    echo "Genesis: CoreState with Operator, guardianGovernance owner1, owner2 and feeCustody owner1, owner2 (2-of-3 each)"
    core_state_transaction="$(submit_interactive "genesis-core-state-$(date +%s)" "$operator_gg_and_fee_custody" \
      "$create_core_state" "$core_state_signers")"
    created_core_state_event="$(jq -ce '[.events[] | .CreatedEvent? // empty | select(.templateId | endswith(":Wormhole.Core.State:CoreState"))]
      | if length == 1 then .[0] else error("expected one created CoreState, found \(length)") end' <<<"$core_state_transaction")"
    check_core_state_pays_fees_to_fee_custody "$created_core_state_event" "$OPERATOR_PARTY" "$GG_PARTY" "$FEE_CUSTODY_PARTY" \
      || exit 1
    echo "CoreState $(created_contract_id "$core_state_transaction" ":Wormhole.Core.State:CoreState") (update $(jq -r .updateId <<<"$core_state_transaction"))"
    ;;
  1)
    echo "CoreState exists; skip"
    active_core_state_event="$(active_contracts "$SUBMITTING_VALIDATOR" "$OPERATOR_PARTY" :Wormhole.Core.State:CoreState \
      | jq -ce --arg operator "$OPERATOR_PARTY" '[.[] | select(.createArgument.operator == $operator)]
        | if length == 1 then .[0] else error("expected one active CoreState, found \(length)") end')"
    check_core_state_pays_fees_to_fee_custody "$active_core_state_event" "$OPERATOR_PARTY" "$GG_PARTY" "$FEE_CUSTODY_PARTY" \
      || exit 1
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
