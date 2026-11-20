#!/usr/bin/env bash
# Register an emitter for CorePublisher, publish one message, then relay one governance VAA (SetMessageFee, chain 75,
# fee 1000, signed by the current guardian set) as GovernanceRelayer. The three local parties submit on sv, with the
# CoreState and EmitterRegistry disclosures that guardianGovernance sees on sv. Each transaction needs
# guardianGovernance confirmations.
# Usage: gg_publish_and_govern.sh [<current-set-key-indices>]   (default 0, the genesis set)
# Precondition: gg_genesis.sh ran and the message fee is 0.
# Run it one time: after it, publishes need a fee allocation, and CorePublisher has its one Emitter.
set -euo pipefail
# shellcheck source=gg_lib.sh
source "$(dirname "${BASH_SOURCE[0]}")/gg_lib.sh"

readonly MESSAGE_PAYLOAD_HEX="63616e746f6e206c6f63616c6e6574"
current_set_key_indices="${1:-0}"
[[ "$current_set_key_indices" =~ ^[0-9]+(,[0-9]+)*$ ]] \
  || { echo "key indices must be comma-separated integers: $current_set_key_indices" >&2; exit 2; }

core_state_argument="$(active_contracts "$CORE_ROLE_VALIDATOR" "$GG_PARTY" :Wormhole.Core.State:CoreState \
  | jq -ec 'if length == 1 then .[0].createArgument else error("expected one CoreState") end')"
current_set_index="$(jq -er .guardianSetIndex <<<"$core_state_argument")"
[[ "$(jq -er .messageFee <<<"$core_state_argument")" == "0" ]] || fail "the message fee is not 0; this script publishes at fee 0"
publisher_only="$(jq -cn --arg party "$CORE_PUBLISHER_PARTY" '[$party]')"

echo "RegisterEmitter (requester = payer = CorePublisher, on $CORE_ROLE_VALIDATOR)"
emitter_registry_cid="$(core_contract_id emitterRegistry "$CORE_ROLE_VALIDATOR")"
register_transaction="$(submit_local "$CORE_ROLE_VALIDATOR" "register-emitter-$(date +%s)" "$publisher_only" \
  "$(exercise_command "$EMITTER_REGISTRY_TEMPLATE" "$emitter_registry_cid" RegisterEmitter \
    "$(jq -n --arg party "$CORE_PUBLISHER_PARTY" --arg gg "$GG_PARTY" \
      '{requester: $party, payer: $party, coreStateCid: null, feeAllocation: null, expectedGuardianGovernance: $gg}')")" \
  "$(jq -cn --argjson disclosure "$(disclosed_contract "$CORE_ROLE_VALIDATOR" "$GG_PARTY" :Wormhole.Core.State:EmitterRegistry "$emitter_registry_cid")" '[$disclosure]')")"
created_contract_id "$register_transaction" ":Wormhole.Core.State:EmitterRegistry" >/dev/null
echo "Emitter $(created_contract_id "$register_transaction" ":Wormhole.Core.State:Emitter")"

echo "PublishMessage (fee 0)"
publish_transaction="$(publish_message "publish-message-$(date +%s)" "$MESSAGE_PAYLOAD_HEX")"
created_contract_id "$publish_transaction" ":Wormhole.Core.State:Emitter" >/dev/null
echo "WormholeMessage: $(jq -c '[.events[] | .ExercisedEvent? // empty | select(.choice == "PublishMessage") | .exerciseResult._2][0]' <<<"$publish_transaction")"

echo "SubmitGovernanceVAA (SetMessageFee 1000) relayed by GovernanceRelayer"
# Governance replay protection is keyed by digest, so each run uses a new sequence.
set_fee_vaa="$(generate_vaa -kind set-message-fee -fee 1000 -signing-set-index "$current_set_index" \
  -signing-key-indices "$current_set_key_indices" -sequence "$(date +%s)")"
governance_transaction="$(relay_governance_vaa "governance-set-fee-$(date +%s)" \
  "$(jq -er .vaaHex <<<"$set_fee_vaa")" "$(vaa_pub_keys_json "$set_fee_vaa")")"
created_contract_id "$governance_transaction" ":Wormhole.Core.State:CoreState" >/dev/null

for validator_name in app-provider sv app-user; do
  echo "$validator_name sees CoreState for guardianGovernance: $(active_contracts "$validator_name" "$GG_PARTY" :Wormhole.Core.State:CoreState \
    | jq -c '[.[] | {contractId: .contractId[0:16], guardianSetIndex: .createArgument.guardianSetIndex, messageFee: .createArgument.messageFee}]')"
done
