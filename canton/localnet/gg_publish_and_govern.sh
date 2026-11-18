#!/usr/bin/env bash
# Register an emitter, publish one message, then apply one governance VAA (SetMessageFee, chain 75,
# fee 1000, signed by guardian set 0). Each transaction needs guardianGovernance confirmations.
# Usage: gg_publish_and_govern.sh
# Precondition: gg_genesis.sh ran and guardian set 0 is current (run it before gg_guardian_set_rotation.sh).
# Run it one time: after it, publishes need a fee allocation.
set -euo pipefail
# shellcheck source=gg_lib.sh
source "$(dirname "${BASH_SOURCE[0]}")/gg_lib.sh"

readonly MESSAGE_PAYLOAD_HEX="63616e746f6e206c6f63616c6e6574"

operator_only="$(jq -n --arg op "$OPERATOR_PARTY" '[$op]')"
core_state_cid="$(core_contract_id coreState)"
current_set_index="$(active_contracts "$SUBMITTING_VALIDATOR" "$OPERATOR_PARTY" :Wormhole.Core.State:CoreState \
  | jq -er 'if length == 1 then .[0].createArgument.guardianSetIndex else error("expected one CoreState") end')"
if [[ "$current_set_index" != "0" ]]; then
  echo "guardian set $current_set_index is current; this script signs with guardian set 0. Run it before gg_guardian_set_rotation.sh." >&2
  exit 1
fi

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
created_contract_id "$publish_transaction" ":Wormhole.Core.State:Emitter" >/dev/null
echo "WormholeMessage: $(jq -c '[.events[] | .ExercisedEvent? // empty | select(.choice == "PublishMessage") | .exerciseResult._2][0]' <<<"$publish_transaction")"

echo "SubmitGovernanceVAA (SetMessageFee 1000)"
# Governance replay protection is keyed by digest, so each run uses a new sequence.
set_fee_vaa="$(generate_vaa -kind set-message-fee -fee 1000 -signing-set-index 0 -signing-key-indices 0 -sequence "$(date +%s)")"
governance_transaction="$(submit_governance_vaa "governance-set-fee-$(date +%s)" \
  "$(jq -er .vaaHex <<<"$set_fee_vaa")" "$(vaa_pub_keys_json "$set_fee_vaa")")"
created_contract_id "$governance_transaction" ":Wormhole.Core.State:CoreState" >/dev/null

for validator_name in app-provider sv app-user; do
  echo "$validator_name sees CoreState for guardianGovernance: $(active_contracts "$validator_name" "$GG_PARTY" :Wormhole.Core.State:CoreState \
    | jq -c '[.[] | {contractId: .contractId[0:16], guardianSetIndex: .createArgument.guardianSetIndex, messageFee: .createArgument.messageFee}]')"
done
