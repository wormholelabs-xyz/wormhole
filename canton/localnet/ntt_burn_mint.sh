#!/usr/bin/env bash
# Run an NTT burn/mint deployment of a guardianGovernance-administered CIP-56 token on LocalNet.
# The "== N." lines of the output name the steps. README.md "NTT end-to-end run" describes them.
# Usage: ntt_burn_mint.sh
# Precondition: ntt_governance.sh ran. Output: the "burnMint" entry of $LOCALNET_STATE_DIR/ntt-contracts.json.
set -euo pipefail
# shellcheck source=ntt_lib.sh
source "$(dirname "${BASH_SOURCE[0]}")/ntt_lib.sh"

readonly PEER_CHAIN_ID=2
readonly PEER_MANAGER_ADDRESS="000000000000000000000000e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0"
readonly PEER_TRANSCEIVER_ADDRESS="000000000000000000000000d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1"
readonly PEER_TOKEN_ADDRESS="000000000000000000000000c2c2c2c2c2c2c2c2c2c2c2c2c2c2c2c2c2c2c2c2"
readonly PEER_RECIPIENT_ADDRESS="000000000000000000000000b3b3b3b3b3b3b3b3b3b3b3b3b3b3b3b3b3b3b3b3"
readonly TOKEN_DECIMALS=8
# Daml Decimal amounts have 10 fraction digits.
readonly DAML_DECIMAL_FRACTION_DIGITS=10
# Inbound 2.5 tokens (wire amount at 8 decimals); outbound burn of 1.0 token.
readonly MINT_TRIMMED_AMOUNT=250000000
readonly MINT_AMOUNT="2.5"
readonly BURN_RAW_AMOUNT=100000000
readonly CHANGE_AMOUNT="1.5"
readonly OUTBOUND_NONCE=7
# Devnet key that signs the negative Mint VAA in place of guardian 0.
readonly WRONG_GUARDIAN_KEY_INDEX=9

run_label="$(date +%s)"
admin_only_signers() { external_party_signers_json "$admin_party" "$admin_key_file" "$admin_fingerprint"; }

echo "== 1. Deployment admin (external party on app-provider)"
admin_json="$(allocate_external_party app-provider "NttBurnMintAdmin-$run_label" "$NTT_KEY_DIR/burn-mint-admin-$run_label")"
admin_party="$(jq -r .partyId <<<"$admin_json")"
admin_key_file="$(jq -r .keyFile <<<"$admin_json")"
admin_fingerprint="$(jq -r .fingerprint <<<"$admin_json")"
echo "admin $admin_party"

instrument_id="$(generate_vaa -kind ntt-instrument-id -admin-party "$admin_party" -instrument-nonce 0 | jq -er .instrumentId)"
instrument_json="$(jq -n --arg gg "$GG_PARTY" --arg id "$instrument_id" '{admin: $gg, id: $id}')"
echo "instrument $instrument_json"

echo "== 2. RegisterManager (BurnMint): admin + guardianGovernance ${NTT_GG_SIGNING_OWNERS[*]}"
governance_cid="$(current_ntt_governance)"
emitter_registry_cid="$(current_emitter_registry)"
replay_root_registry_cid="$(core_contract_id replayRootRegistry)"
core_state_cid="$(core_contract_id coreState)"
coin_factory_cid="$(ntt_contract coinFactory)"
claimed_roots_node_cid="$(registration_claimed_roots_node "$instrument_json" "$admin_party")"
register_transaction="$(submit_guardian_ceremony "ntt-register-burn-mint-$run_label" \
  "$(jq -n --arg admin "$admin_party" --arg gg "$GG_PARTY" '[$admin, $gg]')" \
  "$(exercise_command "$NTT_GOVERNANCE_TEMPLATE" "$governance_cid" RegisterManager "$(jq -n \
    --arg admin "$admin_party" --arg emitterRegistry "$emitter_registry_cid" --arg replayRootRegistry "$replay_root_registry_cid" \
    --arg claimedRootsNode "$claimed_roots_node_cid" \
    --argjson instrument "$instrument_json" --arg factory "$coin_factory_cid" --arg coreState "$core_state_cid" \
    --arg decimals "$TOKEN_DECIMALS" '{
      admin: $admin, chainId: "75", emitterRegistryCid: $emitterRegistry, replayRootRegistryCid: $replayRootRegistry,
      claimedRootsNodeCid: $claimedRootsNode,
      instrumentId: $instrument, instrumentNonce: "0", tokenConfig: "BurnMint", tokenDecimals: $decimals,
      factory: {tag: "BurnMintFactoryCid", value: $factory}, coreStateCid: $coreState, consistencyLevel: "0",
      emitterFeeAllocation: null, claimFeeAllocation: null, feeAllocation: null}')")" \
  "$(jq -s 'add' <(admin_only_signers) <(gg_ceremony_signers_json))" \
  "$(json_array \
    "$(gg_disclosure :Wormhole.Core.State:EmitterRegistry "$emitter_registry_cid")" \
    "$(gg_disclosure :Wormhole.Core.State:ReplayRootRegistry "$replay_root_registry_cid")" \
    "$(gg_disclosure :Wormhole.Core.Replay:ReplayNode "$claimed_roots_node_cid")" \
    "$(gg_disclosure :Wormhole.Core.State:CoreState "$core_state_cid")")")"
register_result="$(exercise_result "$register_transaction" :Wormhole.Ntt.Governance:NttGovernance RegisterManager)"
manager_cid="$(jq -r ._2 <<<"$register_result")"
init_message="$(jq -c ._5 <<<"$register_result")"
manager_json="$(created_contract_argument "$register_transaction" ":Wormhole.Ntt.Manager:NttManager")"
manager_address="$(byte_variant_hex NttManagerAddress "$(jq -c .managerAddress <<<"$manager_json")")"
manager_namespace="$(jq -r .namespace <<<"$manager_json")"
emitter_id="$(jq -r .transceiverEmitterId <<<"$manager_json")"
emitter_address="$(byte_variant_hex EmitterAddress "$(current_emitter "$emitter_id" | jq -c .createArgument.emitterAddress)")"
echo "manager $manager_cid address $manager_address emitterId $emitter_id emitterAddress $emitter_address"
echo "TransceiverInit message $(message_summary "$init_message")"
if [[ "$(byte_variant_hex MessagePayload "$(jq -c .payload <<<"$init_message")" | cut -c1-8)" != "9c23bd3b" ]]; then
  echo "RegisterManager did not publish TransceiverInit" >&2
  exit 1
fi

echo "== 3. SetPeer chain $PEER_CHAIN_ID (admin)"
emitter_cid="$(current_emitter "$emitter_id" | jq -r .contractId)"
set_peer_transaction="$(submit_interactive "ntt-burn-mint-set-peer-$run_label" "$(jq -n --arg admin "$admin_party" '[$admin]')" \
  "$(exercise_command "$NTT_MANAGER_TEMPLATE" "$manager_cid" SetPeer "$(jq -n --arg chain "$PEER_CHAIN_ID" \
    --argjson manager "$(byte_variant_json NttManagerAddress "$PEER_MANAGER_ADDRESS")" \
    --argjson transceiver "$(byte_variant_json EmitterAddress "$PEER_TRANSCEIVER_ADDRESS")" --arg emitter "$emitter_cid" \
    --arg coreState "$core_state_cid" --arg admin "$admin_party" '{
      chainId: $chain, peer: {managerAddress: $manager, transceiverAddress: $transceiver, decimals: "8"},
      transceiverEmitterCid: $emitter, coreStateCid: $coreState, consistencyLevel: "0", payer: $admin, feeAllocation: null}')")" \
  "$(admin_only_signers)" \
  "$(json_array "$(gg_disclosure :Wormhole.Core.State:Emitter "$emitter_cid")" \
    "$(gg_disclosure :Wormhole.Core.State:CoreState "$core_state_cid")")")"
set_peer_result="$(exercise_result "$set_peer_transaction" :Wormhole.Ntt.Manager:NttManager SetPeer)"
manager_cid="$(jq -r ._1 <<<"$set_peer_result")"
echo "TransceiverRegistration message $(message_summary "$(jq -c ._2 <<<"$set_peer_result")")"

echo "== 4. User on app-user creates a DepositPreapproval"
user_party="$(allocate_local_party app-user "NttBurnMintUser-$run_label")"
user_only="$(jq -n --arg user "$user_party" '[$user]')"
preapproval_transaction="$(submit_local app-user "ntt-deposit-preapproval-$run_label" "$user_only" \
  "$(create_command "$DEPOSIT_PREAPPROVAL_TEMPLATE" "$(jq -n --arg user "$user_party" --argjson instrument "$instrument_json" \
    '{owner: $user, instrumentId: $instrument}')")")"
preapproval_cid="$(created_contract_id "$preapproval_transaction" ":Wormhole.Ntt.Deposit:DepositPreapproval")"
echo "user $user_party preapproval $preapproval_cid"

echo "== 5. Relayer on sv delivers the inbound transfer with Mint"
relayer_party="$(allocate_local_party sv "NttRelayer-$run_label")"
# mint_disclosures <replay-node-cid>
mint_disclosures() {
  json_array \
    "$(gg_disclosure :Wormhole.Ntt.Manager:NttManager "$manager_cid")" \
    "$(gg_disclosure :Wormhole.Core.State:CoreState "$core_state_cid")" \
    "$(gg_disclosure :Wormhole.Core.Replay:ReplayNode "$1")" \
    "$(disclosed_contract "$SUBMITTING_VALIDATOR" "$GG_PARTY" :Wormhole.Ntt.Deposit:DepositPreapproval "$preapproval_cid")" \
    "$(disclosed_contract "$SUBMITTING_VALIDATOR" "$GG_PARTY" :Token.CIP0056.CoinFactory:CoinFactory "$coin_factory_cid")"
}
# mint_command <vaa-json> <replay-node-cid>
mint_command() {
  exercise_command "$NTT_MANAGER_TEMPLATE" "$manager_cid" Mint "$(jq -n --arg relayer "$relayer_party" \
    --arg coreState "$core_state_cid" --arg replayNode "$2" --arg vaa "$(jq -r .vaaHex <<<"$1")" \
    --arg user "$user_party" --arg preapproval "$preapproval_cid" \
    --argjson extraArgs "$EMPTY_EXTRA_ARGS" '{
      executor: $relayer, coreStateCid: $coreState, replayNodeCid: $replayNode, vaaBytes: {tag: "EncodedVaa", value: $vaa},
      recipient: $user, depositPreapprovalCid: $preapproval, extraArgs: $extraArgs}')"
}
relayer_only="$(jq -n --arg relayer "$relayer_party" '[$relayer]')"
mint_vaa="$(ntt_transfer_vaa -sequence "$run_label" -emitter-chain "$PEER_CHAIN_ID" \
  -emitter-address "$PEER_TRANSCEIVER_ADDRESS" -source-manager "$PEER_MANAGER_ADDRESS" -recipient-manager "$manager_address" \
  -source-token "$PEER_TOKEN_ADDRESS" -recipient-party "$user_party" -ntt-message-id "$run_label" \
  -trimmed-decimals 8 -trimmed-amount "$MINT_TRIMMED_AMOUNT")"
replay_node_cid="$(covering_replay_node "$manager_namespace" "$(jq -r .digestHex <<<"$mint_vaa")")"
mint_transaction="$(submit_local sv "ntt-mint-$run_label" "$relayer_only" \
  "$(mint_command "$mint_vaa" "$replay_node_cid")" "$(mint_disclosures "$replay_node_cid")")"
echo "Mint decoded transfer $(exercise_result "$mint_transaction" :Wormhole.Ntt.Manager:NttManager Mint | jq -c '{decimals, amount, recipientChain}')"
user_coins="$(active_contracts app-user "$user_party" ":Token.CIP0056.Coin:Coin" \
  | jq -c '[.[] | {contractId, amount: .createArgument.amount, owner: .createArgument.owner, instrumentId: .createArgument.instrumentId}]')"
echo "user coins after Mint: $(jq -c '[.[].amount]' <<<"$user_coins")"
# guardianGovernance must be the instrument admin, thus a signatory, of each minted Coin.
if ! jq -e --arg gg "$GG_PARTY" --arg id "$instrument_id" --arg user "$user_party" \
  'length == 1 and all(.[]; .instrumentId.admin == $gg and .instrumentId.id == $id and .owner == $user)' \
  <<<"$user_coins" >/dev/null; then
  echo "expected one Coin of instrument {admin: guardianGovernance, id: $instrument_id} owned by the user: $user_coins" >&2
  exit 1
fi
user_coin_units="$(json_decimal_array_total_units "$DAML_DECIMAL_FRACTION_DIGITS" "$(jq -c 'map(.amount)' <<<"$user_coins")")"
if [[ "$user_coin_units" != "$(decimal_to_units "$MINT_AMOUNT" "$DAML_DECIMAL_FRACTION_DIGITS")" ]]; then
  echo "expected the user to hold $MINT_AMOUNT after Mint, holds $user_coin_units units" >&2
  exit 1
fi

echo "== 5a. The same Mint VAA again is rejected"
replay_node_cid="$(covering_replay_node "$manager_namespace" "$(jq -r .digestHex <<<"$mint_vaa")")"
submit_local_expect_rejection sv "ntt-mint-replay-$run_label" "$relayer_only" \
  "$(mint_command "$mint_vaa" "$replay_node_cid")" "$(mint_disclosures "$replay_node_cid")" "digest already consumed"

echo "== 5b. A Mint VAA signed by devnet key $WRONG_GUARDIAN_KEY_INDEX for guardian 0 is rejected"
IFS=, read -r -a wrong_signing_key_indices <<<"$NTT_GUARDIAN_KEY_INDICES"
for key_index in "${wrong_signing_key_indices[@]}"; do
  if [[ "$key_index" == "$WRONG_GUARDIAN_KEY_INDEX" ]]; then
    echo "NTT_GUARDIAN_KEY_INDICES contains $WRONG_GUARDIAN_KEY_INDEX; the negative case needs a key outside the set" >&2
    exit 1
  fi
done
wrong_signing_key_indices[0]="$WRONG_GUARDIAN_KEY_INDEX"
wrong_key_vaa="$(generate_vaa -kind ntt-transfer -signing-set-index "$NTT_GUARDIAN_SET_INDEX" \
  -signing-key-indices "$(IFS=,; echo "${wrong_signing_key_indices[*]}")" -sequence "$((run_label + 1))" \
  -emitter-chain "$PEER_CHAIN_ID" -emitter-address "$PEER_TRANSCEIVER_ADDRESS" -source-manager "$PEER_MANAGER_ADDRESS" \
  -recipient-manager "$manager_address" -source-token "$PEER_TOKEN_ADDRESS" -recipient-party "$user_party" \
  -ntt-message-id "$((run_label + 1))" -trimmed-decimals 8 -trimmed-amount "$MINT_TRIMMED_AMOUNT")"
replay_node_cid="$(covering_replay_node "$manager_namespace" "$(jq -r .digestHex <<<"$wrong_key_vaa")")"
submit_local_expect_rejection sv "ntt-mint-wrong-key-$run_label" "$relayer_only" \
  "$(mint_command "$wrong_key_vaa" "$replay_node_cid")" "$(mint_disclosures "$replay_node_cid")" \
  "invalid signature for guardian index 0"

echo "== 6. User burns with Transfer to chain $PEER_CHAIN_ID"
emitter_cid="$(current_emitter "$emitter_id" | jq -r .contractId)"
burn_transaction="$(submit_local app-user "ntt-burn-$run_label" "$user_only" \
  "$(exercise_command "$NTT_MANAGER_TEMPLATE" "$manager_cid" Transfer "$(jq -n --arg user "$user_party" \
    --arg emitter "$emitter_cid" --arg coreState "$core_state_cid" --arg chain "$PEER_CHAIN_ID" \
    --argjson recipient "$(byte_variant_json UniversalAddress "$PEER_RECIPIENT_ADDRESS")" --arg rawAmount "$BURN_RAW_AMOUNT" --arg nonce "$OUTBOUND_NONCE" \
    --argjson inputs "$(jq -c '[.[].contractId]' <<<"$user_coins")" --argjson extraArgs "$EMPTY_EXTRA_ARGS" '{
      user: $user, transceiverEmitterCid: $emitter, coreStateCid: $coreState, ledgerCid: null,
      recipientChain: $chain, recipientAddress: $recipient, rawAmount: $rawAmount, nonce: $nonce, consistencyLevel: "0",
      inputHoldingCids: $inputs, feeAllocation: null, extraArgs: $extraArgs}')")" \
  "$(json_array \
    "$(gg_disclosure :Wormhole.Ntt.Manager:NttManager "$manager_cid")" \
    "$(gg_disclosure :Wormhole.Core.State:Emitter "$emitter_cid")" \
    "$(gg_disclosure :Wormhole.Core.State:CoreState "$core_state_cid")" \
    "$(disclosed_contract "$SUBMITTING_VALIDATOR" "$GG_PARTY" :Token.CIP0056.CoinFactory:CoinFactory "$coin_factory_cid")")")"
burn_message="$(exercise_result "$burn_transaction" :Wormhole.Ntt.Manager:NttManager Transfer)"
echo "outbound NTT message $(message_summary "$burn_message") update $(jq -r .updateId <<<"$burn_transaction")"
echo "payload $(byte_variant_hex MessagePayload "$(jq -c .payload <<<"$burn_message")")"
user_change_coins="$(active_contracts app-user "$user_party" ":Token.CIP0056.Coin:Coin" | jq -c '[.[].createArgument]')"
user_change="$(jq -c 'map(.amount)' <<<"$user_change_coins")"
echo "user coins after burn: $user_change"
if ! jq -e --arg gg "$GG_PARTY" --arg id "$instrument_id" \
  'length >= 1 and all(.[]; .instrumentId.admin == $gg and .instrumentId.id == $id)' <<<"$user_change_coins" >/dev/null; then
  echo "expected change Coins of instrument {admin: guardianGovernance, id: $instrument_id}: $user_change_coins" >&2
  exit 1
fi
user_change_units="$(json_decimal_array_total_units "$DAML_DECIMAL_FRACTION_DIGITS" "$user_change")"
if [[ "$user_change_units" != "$(decimal_to_units "$CHANGE_AMOUNT" "$DAML_DECIMAL_FRACTION_DIGITS")" ]]; then
  echo "expected the user to hold $CHANGE_AMOUNT after the burn, holds $user_change_units units" >&2
  exit 1
fi

record_ntt_json burnMint "$(jq -nc --arg admin "$admin_party" --arg manager "$manager_cid" \
  --arg managerAddress "$manager_address" --arg emitterId "$emitter_id" --arg emitterAddress "$emitter_address" \
  --arg user "$user_party" --arg burnUpdateId "$(jq -r .updateId <<<"$burn_transaction")" \
  --argjson burnMessage "$burn_message" '{admin: $admin, managerCid: $manager, managerAddress: $managerAddress,
    emitterId: $emitterId, emitterAddress: $emitterAddress, user: $user, burnUpdateId: $burnUpdateId, burnMessage: $burnMessage}')"
echo "burn/mint deployment recorded in $NTT_CONTRACTS"
