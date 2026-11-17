#!/usr/bin/env bash
# NTT burn/mint deployment of a guardianGovernance-administered CIP-56 token on LocalNet:
#   1. Allocate an external deployment admin on app-provider.
#   2. RegisterManager (BurnMint) with the admin and the guardianGovernance ceremony. It publishes TransceiverInit.
#   3. SetPeer for the Ethereum peer (chain 2). It publishes TransceiverRegistration.
#   4. A user on app-user creates a DepositPreapproval.
#   5. A relayer on sv delivers a guardian-signed inbound transfer VAA with Mint. The user receives the token.
#   6. The user burns part of the token with Transfer. The transceiver publishes the outbound NTT message.
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
# Inbound 2.5 tokens (wire amount at 8 decimals); outbound burn of 1.0 token.
readonly MINT_TRIMMED_AMOUNT=250000000
readonly MINT_AMOUNT="2.5"
readonly BURN_RAW_AMOUNT=100000000
readonly CHANGE_AMOUNT="1.5"
readonly OUTBOUND_NONCE=7

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
register_transaction="$(submit_interactive "ntt-register-burn-mint-$run_label" \
  "$(jq -n --arg admin "$admin_party" --arg gg "$GG_PARTY" '[$admin, $gg]')" \
  "$(exercise_command "$NTT_GOVERNANCE_TEMPLATE" "$governance_cid" RegisterManager "$(jq -n \
    --arg admin "$admin_party" --arg emitterRegistry "$emitter_registry_cid" --arg replayRootRegistry "$replay_root_registry_cid" \
    --argjson instrument "$instrument_json" --arg factory "$coin_factory_cid" --arg coreState "$core_state_cid" \
    --arg decimals "$TOKEN_DECIMALS" '{
      admin: $admin, chainId: "75", emitterRegistryCid: $emitterRegistry, replayRootRegistryCid: $replayRootRegistry,
      instrumentId: $instrument, instrumentNonce: "0", tokenConfig: "BurnMint", tokenDecimals: $decimals,
      factory: {tag: "BurnMintFactoryCid", value: $factory}, coreStateCid: $coreState, consistencyLevel: "0",
      emitterFeeAllocation: null, claimFeeAllocation: null, feeAllocation: null}')")" \
  "$(jq -s 'add' <(admin_only_signers) <(gg_ceremony_signers_json))" \
  "$(json_array \
    "$(operator_disclosure :Wormhole.Core.State:EmitterRegistry "$emitter_registry_cid")" \
    "$(operator_disclosure :Wormhole.Core.State:ReplayRootRegistry "$replay_root_registry_cid")" \
    "$(operator_disclosure :Wormhole.Core.State:CoreState "$core_state_cid")")")"
register_result="$(exercise_result "$register_transaction" RegisterManager)"
manager_cid="$(jq -r ._2 <<<"$register_result")"
init_message="$(jq -c ._5 <<<"$register_result")"
manager_json="$(created_contract_argument "$register_transaction" ":Wormhole.Ntt.Manager:NttManager")"
manager_address="$(jq -r .managerAddress <<<"$manager_json")"
manager_namespace="$(jq -r .namespace <<<"$manager_json")"
emitter_id="$(jq -r .transceiverEmitterId <<<"$manager_json")"
emitter_address="$(current_emitter "$emitter_id" | jq -r .createArgument.emitterAddress)"
echo "manager $manager_cid address $manager_address emitterId $emitter_id emitterAddress $emitter_address"
echo "TransceiverInit message $(message_summary "$init_message")"
if [[ "$(jq -r .payload[0:8] <<<"$init_message")" != "9c23bd3b" ]]; then
  echo "RegisterManager did not publish TransceiverInit" >&2
  exit 1
fi

echo "== 3. SetPeer chain $PEER_CHAIN_ID (admin)"
emitter_cid="$(current_emitter "$emitter_id" | jq -r .contractId)"
set_peer_transaction="$(submit_interactive "ntt-burn-mint-set-peer-$run_label" "$(jq -n --arg admin "$admin_party" '[$admin]')" \
  "$(exercise_command "$NTT_MANAGER_TEMPLATE" "$manager_cid" SetPeer "$(jq -n --arg chain "$PEER_CHAIN_ID" \
    --arg manager "$PEER_MANAGER_ADDRESS" --arg transceiver "$PEER_TRANSCEIVER_ADDRESS" --arg emitter "$emitter_cid" \
    --arg coreState "$core_state_cid" --arg admin "$admin_party" '{
      chainId: $chain, peer: {managerAddress: $manager, transceiverAddress: $transceiver, decimals: "8"},
      transceiverEmitterCid: $emitter, coreStateCid: $coreState, consistencyLevel: "0", payer: $admin, feeAllocation: null}')")" \
  "$(admin_only_signers)" \
  "$(json_array "$(operator_disclosure :Wormhole.Core.State:Emitter "$emitter_cid")" \
    "$(operator_disclosure :Wormhole.Core.State:CoreState "$core_state_cid")")")"
set_peer_result="$(exercise_result "$set_peer_transaction" SetPeer)"
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
mint_vaa="$(ntt_transfer_vaa -sequence "$run_label" -emitter-chain "$PEER_CHAIN_ID" \
  -emitter-address "$PEER_TRANSCEIVER_ADDRESS" -source-manager "$PEER_MANAGER_ADDRESS" -recipient-manager "$manager_address" \
  -source-token "$PEER_TOKEN_ADDRESS" -recipient-party "$user_party" -ntt-message-id "$run_label" \
  -trimmed-decimals 8 -trimmed-amount "$MINT_TRIMMED_AMOUNT")"
replay_node_cid="$(covering_replay_node "$manager_namespace" "$(jq -r .digestHex <<<"$mint_vaa")")"
mint_transaction="$(submit_local sv "ntt-mint-$run_label" "$(jq -n --arg relayer "$relayer_party" '[$relayer]')" \
  "$(exercise_command "$NTT_MANAGER_TEMPLATE" "$manager_cid" Mint "$(jq -n --arg relayer "$relayer_party" \
    --arg coreState "$core_state_cid" --arg replayNode "$replay_node_cid" --arg vaa "$(jq -r .vaaHex <<<"$mint_vaa")" \
    --argjson keys "$(vaa_pub_keys_json "$mint_vaa")" --arg user "$user_party" --arg preapproval "$preapproval_cid" \
    --argjson extraArgs "$EMPTY_EXTRA_ARGS" '{
      executor: $relayer, coreStateCid: $coreState, replayNodeCid: $replayNode, vaaBytes: $vaa, pubKeys: $keys,
      recipient: $user, depositPreapprovalCid: $preapproval, extraArgs: $extraArgs}')")" \
  "$(json_array \
    "$(operator_disclosure :Wormhole.Ntt.Manager:NttManager "$manager_cid")" \
    "$(operator_disclosure :Wormhole.Core.State:CoreState "$core_state_cid")" \
    "$(operator_disclosure :Wormhole.Core.Replay:ReplayNode "$replay_node_cid")" \
    "$(disclosed_contract "$SUBMITTING_VALIDATOR" "$GG_PARTY" :Wormhole.Ntt.Deposit:DepositPreapproval "$preapproval_cid")" \
    "$(disclosed_contract "$SUBMITTING_VALIDATOR" "$GG_PARTY" :Token.CIP0056.CoinFactory:CoinFactory "$coin_factory_cid")")")"
echo "Mint decoded transfer $(exercise_result "$mint_transaction" Mint | jq -c '{decimals, amount, recipientChain}')"
user_coins="$(active_contracts app-user "$user_party" ":Token.CIP0056.Coin:Coin" | jq -c '[.[] | {contractId, amount: .createArgument.amount}]')"
echo "user coins after Mint: $(jq -c '[.[].amount]' <<<"$user_coins")"
if ! jq -e --argjson expected "$MINT_AMOUNT" 'map(.amount | tonumber) | add == $expected' <<<"$user_coins" >/dev/null; then
  echo "expected the user to hold $MINT_AMOUNT after Mint" >&2
  exit 1
fi

echo "== 6. User burns with Transfer to chain $PEER_CHAIN_ID"
emitter_cid="$(current_emitter "$emitter_id" | jq -r .contractId)"
burn_transaction="$(submit_local app-user "ntt-burn-$run_label" "$user_only" \
  "$(exercise_command "$NTT_MANAGER_TEMPLATE" "$manager_cid" Transfer "$(jq -n --arg user "$user_party" \
    --arg emitter "$emitter_cid" --arg coreState "$core_state_cid" --arg chain "$PEER_CHAIN_ID" \
    --arg recipient "$PEER_RECIPIENT_ADDRESS" --arg rawAmount "$BURN_RAW_AMOUNT" --arg nonce "$OUTBOUND_NONCE" \
    --argjson inputs "$(jq -c '[.[].contractId]' <<<"$user_coins")" --argjson extraArgs "$EMPTY_EXTRA_ARGS" '{
      user: $user, transceiverEmitterCid: $emitter, coreStateCid: $coreState, ledgerCid: null,
      recipientChain: $chain, recipientAddress: $recipient, rawAmount: $rawAmount, nonce: $nonce, consistencyLevel: "0",
      inputHoldingCids: $inputs, feeAllocation: null, extraArgs: $extraArgs}')")" \
  "$(json_array \
    "$(operator_disclosure :Wormhole.Ntt.Manager:NttManager "$manager_cid")" \
    "$(operator_disclosure :Wormhole.Core.State:Emitter "$emitter_cid")" \
    "$(operator_disclosure :Wormhole.Core.State:CoreState "$core_state_cid")" \
    "$(disclosed_contract "$SUBMITTING_VALIDATOR" "$GG_PARTY" :Token.CIP0056.CoinFactory:CoinFactory "$coin_factory_cid")")")"
burn_message="$(exercise_result "$burn_transaction" Transfer)"
echo "outbound NTT message $(message_summary "$burn_message") update $(jq -r .updateId <<<"$burn_transaction")"
echo "payload $(jq -r .payload <<<"$burn_message")"
user_change="$(active_contracts app-user "$user_party" ":Token.CIP0056.Coin:Coin" | jq -c '[.[].createArgument.amount]')"
echo "user coins after burn: $user_change"
if ! jq -e --argjson expected "$CHANGE_AMOUNT" 'map(tonumber) | add == $expected' <<<"$user_change" >/dev/null; then
  echo "expected the user to hold $CHANGE_AMOUNT after the burn" >&2
  exit 1
fi

record_ntt_json burnMint "$(jq -nc --arg admin "$admin_party" --arg manager "$manager_cid" \
  --arg managerAddress "$manager_address" --arg emitterId "$emitter_id" --arg emitterAddress "$emitter_address" \
  --arg user "$user_party" --arg burnUpdateId "$(jq -r .updateId <<<"$burn_transaction")" \
  --argjson burnMessage "$burn_message" '{admin: $admin, managerCid: $manager, managerAddress: $managerAddress,
    emitterId: $emitterId, emitterAddress: $emitterAddress, user: $user, burnUpdateId: $burnUpdateId, burnMessage: $burnMessage}')"
echo "burn/mint deployment recorded in $NTT_CONTRACTS"
