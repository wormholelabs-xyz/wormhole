#!/usr/bin/env bash
# Register an emitter, publish one message, then apply one governance VAA (SetMessageFee, chain 75,
# fee 1000, signed by guardian set 0). Each transaction needs guardianGovernance confirmations.
# Usage: gg_publish_and_govern.sh
# Precondition: gg_genesis.sh ran. Run it one time: after it, publishes need a fee allocation.
set -euo pipefail
# shellcheck source=gg_lib.sh
source "$(dirname "${BASH_SOURCE[0]}")/gg_lib.sh"

# SetMessageFee(chain 75, fee 1000) from Test.TestCore.govSetFeeVAA in the wt/core worktree.
readonly GOV_SET_FEE_VAA="01000000000100797d5065f8fef799fdfb8e1042eccc143a77b35a4d9d2689474cdbf8500bffe46678c27b114d015c45f175fb5bd687cff92c4238d31035bacb65ddf8ae99b6f8006553f100000000000001000000000000000000000000000000000000000000000000000000000000000400000000000000010000000000000000000000000000000000000000000000000000000000436f726503004b00000000000000000000000000000000000000000000000000000000000003e8"
readonly DEVNET_GUARDIAN_PUBLIC_KEY="04d4a4629979f0c9fa0f0bb54edf33f87c8c5a1f42c0350a30d68f7e967023e34e495a8ebf5101036d0fd66e3b0a8c7c61b65fceeaf487ab3cd1b5b7b50beb7970"
readonly MESSAGE_PAYLOAD_HEX="63616e746f6e206c6f63616c6e6574"

operator_only="$(jq -n --arg op "$OPERATOR_PARTY" '[$op]')"
core_state_cid="$(core_contract_id coreState)"

echo "RegisterEmitter (requester = payer = Operator)"
register_transaction="$(submit_interactive "register-emitter-$(date +%s)" "$operator_only" \
  "$(exercise_command "$EMITTER_REGISTRY_TEMPLATE" "$(core_contract_id emitterRegistry)" RegisterEmitter \
    "$(jq -n --arg op "$OPERATOR_PARTY" --arg gg "$GG_PARTY" \
      '{requester: $op, payer: $op, coreStateCid: null, feeAllocation: null, expectedGuardianGovernance: $gg}')")" \
  "$(operator_signers_json)")"
record_core_contract_id emitterRegistry "$(created_contract_id "$register_transaction" ":Wormhole.Core.State:EmitterRegistry")"
emitter_cid="$(created_contract_id "$register_transaction" ":Wormhole.Core.State:Emitter")"

echo "PublishMessage (fee 0)"
publish_transaction="$(submit_interactive "publish-message-$(date +%s)" "$operator_only" \
  "$(exercise_command "$EMITTER_TEMPLATE" "$emitter_cid" PublishMessage \
    "$(jq -n --arg op "$OPERATOR_PARTY" --arg cs "$core_state_cid" --arg payload "$MESSAGE_PAYLOAD_HEX" \
      '{nonce: "1", payload: $payload, consistencyLevel: "0", coreStateCid: $cs, payer: $op, feeAllocation: null}')")" \
  "$(operator_signers_json)")"
record_core_contract_id emitter "$(created_contract_id "$publish_transaction" ":Wormhole.Core.State:Emitter")"
echo "WormholeMessage: $(jq -c '[.events[] | .ExercisedEvent? // empty | select(.choice == "PublishMessage") | .exerciseResult._2][0]' <<<"$publish_transaction")"

echo "SubmitGovernanceVAA (SetMessageFee 1000)"
governance_transaction="$(submit_interactive "governance-set-fee-$(date +%s)" "$operator_only" \
  "$(exercise_command "$CORE_STATE_TEMPLATE" "$core_state_cid" SubmitGovernanceVAA \
    "$(jq -n --arg vaa "$GOV_SET_FEE_VAA" --arg key "$DEVNET_GUARDIAN_PUBLIC_KEY" '{encodedVAA: $vaa, pubKeys: [{_1: "0", _2: $key}]}')")" \
  "$(operator_signers_json)")"
new_core_state_cid="$(created_contract_id "$governance_transaction" ":Wormhole.Core.State:CoreState")"
record_core_contract_id coreState "$new_core_state_cid"

for validator_name in app-provider sv app-user; do
  echo "$validator_name sees CoreState for guardianGovernance: $(active_contracts "$validator_name" "$GG_PARTY" :Wormhole.Core.State:CoreState \
    | jq -c '[.[] | {contractId: .contractId[0:16], guardianSetIndex: .createArgument.guardianSetIndex, messageFee: .createArgument.messageFee}]')"
done
