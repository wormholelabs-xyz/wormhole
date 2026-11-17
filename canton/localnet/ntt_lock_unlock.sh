#!/usr/bin/env bash
# NTT lock/unlock deployment of Canton Coin (Amulet) on LocalNet. The deployment admin is the custodian.
#   1. Onboard wallet users: the custodian (app-provider), a locker and a recipient (app-user). Each taps Canton Coin.
#      The custodian and the recipient create Amulet TransferPreapprovals through the wallet API.
#   2. RegisterManager (LockUnlock) by the custodian alone, with the Amulet registry transfer factory.
#   3. SetPeer for the Ethereum peer (chain 2).
#   4. The locker locks Canton Coin with Transfer. The custodian receives it and the LockedLedger is credited.
#   5. A relayer on sv delivers a guardian-signed inbound transfer VAA with Release. The recipient receives
#      Canton Coin through its TransferPreapproval and the LockedLedger is debited.
# Usage: ntt_lock_unlock.sh
# Precondition: ntt_governance.sh ran. Output: the "lockUnlock" entry of $LOCALNET_STATE_DIR/ntt-contracts.json.
set -euo pipefail
# shellcheck source=ntt_lib.sh
source "$(dirname "${BASH_SOURCE[0]}")/ntt_lib.sh"

readonly PEER_CHAIN_ID=2
readonly PEER_MANAGER_ADDRESS="000000000000000000000000e4e4e4e4e4e4e4e4e4e4e4e4e4e4e4e4e4e4e4e4"
readonly PEER_TRANSCEIVER_ADDRESS="000000000000000000000000d5d5d5d5d5d5d5d5d5d5d5d5d5d5d5d5d5d5d5d5"
readonly PEER_TOKEN_ADDRESS="000000000000000000000000c6c6c6c6c6c6c6c6c6c6c6c6c6c6c6c6c6c6c6c6"
readonly PEER_RECIPIENT_ADDRESS="000000000000000000000000b7b7b7b7b7b7b7b7b7b7b7b7b7b7b7b7b7b7b7b7"
# Amulet has 10 decimals. Lock 5 CC; release 2 CC (wire amount at 8 decimals).
readonly AMULET_DECIMALS=10
readonly LOCK_AMOUNT="5.0"
readonly LOCK_RAW_AMOUNT=50000000000
readonly RELEASE_AMOUNT="2.0"
readonly RELEASE_TRIMMED_AMOUNT=200000000
readonly LEDGER_BALANCE_AFTER_RELEASE="3.0"
readonly TAP_AMOUNT_USD="10.0"
readonly OUTBOUND_NONCE=8

run_label="$(date +%s)"

amulet_total() {
  amulet_holdings "$1" "$2" | jq -r 'map(.amount | tonumber) | add // 0'
}

echo "== 1. Wallet users, Canton Coin and TransferPreapprovals"
custodian_user="ntt-custodian-$run_label"
locker_user="ntt-locker-$run_label"
recipient_user="ntt-recipient-$run_label"
custodian_party="$(wallet_onboard app-provider "$custodian_user")"
locker_party="$(wallet_onboard app-user "$locker_user")"
recipient_party="$(wallet_onboard app-user "$recipient_user")"
wallet_tap app-provider "$custodian_user" "$TAP_AMOUNT_USD"
wallet_tap app-user "$locker_user" "$TAP_AMOUNT_USD"
wallet_tap app-user "$recipient_user" "$TAP_AMOUNT_USD"
custodian_preapproval_cid="$(wallet_create_transfer_preapproval app-provider "$custodian_user")"
recipient_preapproval_cid="$(wallet_create_transfer_preapproval app-user "$recipient_user")"
echo "custodian $custodian_party (TransferPreapproval ${custodian_preapproval_cid:0:16})"
echo "locker    $locker_party"
echo "recipient $recipient_party (TransferPreapproval ${recipient_preapproval_cid:0:16})"
relayer_party="$(allocate_local_party sv "NttRelayer-$run_label")"
echo "relayer   $relayer_party"
custodian_only="$(jq -n --arg party "$custodian_party" '[$party]')"

dso="$(dso_party)"
amulet_instrument="$(jq -n --arg dso "$dso" '{admin: $dso, id: "Amulet"}')"
locker_inputs="$(amulet_holdings app-user "$locker_party" | jq -c '[.[].contractId]')"
lock_registry="$(amulet_transfer_factory "$locker_party" "$custodian_party" "$LOCK_AMOUNT" "$locker_inputs")"
transfer_factory_cid="$(jq -er .factoryId <<<"$lock_registry")"
echo "Amulet transfer factory ${transfer_factory_cid:0:16}, locker-to-custodian kind $(jq -r .transferKind <<<"$lock_registry")"

echo "== 2. RegisterManager (LockUnlock) by the custodian"
governance_cid="$(current_ntt_governance)"
emitter_registry_cid="$(current_emitter_registry)"
replay_root_registry_cid="$(core_contract_id replayRootRegistry)"
core_state_cid="$(core_contract_id coreState)"
register_transaction="$(submit_local app-provider "ntt-register-lock-unlock-$run_label" "$custodian_only" \
  "$(exercise_command "$NTT_GOVERNANCE_TEMPLATE" "$governance_cid" RegisterManager "$(jq -n \
    --arg admin "$custodian_party" --arg emitterRegistry "$emitter_registry_cid" --arg replayRootRegistry "$replay_root_registry_cid" \
    --argjson instrument "$amulet_instrument" --arg factory "$transfer_factory_cid" --arg coreState "$core_state_cid" \
    --arg decimals "$AMULET_DECIMALS" '{
      admin: $admin, chainId: "75", emitterRegistryCid: $emitterRegistry, replayRootRegistryCid: $replayRootRegistry,
      instrumentId: $instrument, instrumentNonce: "0", tokenConfig: "LockUnlock", tokenDecimals: $decimals,
      factory: {tag: "TransferFactoryCid", value: $factory}, coreStateCid: $coreState, consistencyLevel: "0",
      emitterFeeAllocation: null, claimFeeAllocation: null, feeAllocation: null}')")" \
  "$(json_array \
    "$(operator_disclosure :Wormhole.Ntt.Governance:NttGovernance "$governance_cid")" \
    "$(operator_disclosure :Wormhole.Core.State:EmitterRegistry "$emitter_registry_cid")" \
    "$(operator_disclosure :Wormhole.Core.State:ReplayRootRegistry "$replay_root_registry_cid")" \
    "$(operator_disclosure :Wormhole.Core.State:CoreState "$core_state_cid")")")"
register_result="$(exercise_result "$register_transaction" RegisterManager)"
manager_cid="$(jq -r ._2 <<<"$register_result")"
ledger_cid="$(jq -r ._4 <<<"$register_result")"
manager_json="$(created_contract_argument "$register_transaction" ":Wormhole.Ntt.Manager:NttManager")"
manager_address="$(jq -r .managerAddress <<<"$manager_json")"
manager_namespace="$(jq -r .namespace <<<"$manager_json")"
emitter_id="$(jq -r .transceiverEmitterId <<<"$manager_json")"
emitter_address="$(current_emitter "$emitter_id" | jq -r .createArgument.emitterAddress)"
echo "manager ${manager_cid:0:16} address $manager_address emitterId $emitter_id emitterAddress $emitter_address"
echo "LockedLedger ${ledger_cid:0:16} $(created_contract_argument "$register_transaction" ":Wormhole.Ntt.Ledger:LockedLedger" | jq -c '{admin: .admin[0:24], balance}')"
echo "TransceiverInit message $(message_summary "$(jq -c ._5 <<<"$register_result")")"

echo "== 3. SetPeer chain $PEER_CHAIN_ID (custodian)"
emitter_cid="$(current_emitter "$emitter_id" | jq -r .contractId)"
set_peer_transaction="$(submit_local app-provider "ntt-lock-unlock-set-peer-$run_label" "$custodian_only" \
  "$(exercise_command "$NTT_MANAGER_TEMPLATE" "$manager_cid" SetPeer "$(jq -n --arg chain "$PEER_CHAIN_ID" \
    --arg manager "$PEER_MANAGER_ADDRESS" --arg transceiver "$PEER_TRANSCEIVER_ADDRESS" --arg emitter "$emitter_cid" \
    --arg coreState "$core_state_cid" --arg admin "$custodian_party" '{
      chainId: $chain, peer: {managerAddress: $manager, transceiverAddress: $transceiver, decimals: "8"},
      transceiverEmitterCid: $emitter, coreStateCid: $coreState, consistencyLevel: "0", payer: $admin, feeAllocation: null}')")" \
  "$(json_array "$(operator_disclosure :Wormhole.Core.State:Emitter "$emitter_cid")" \
    "$(operator_disclosure :Wormhole.Core.State:CoreState "$core_state_cid")")")"
manager_cid="$(exercise_result "$set_peer_transaction" SetPeer | jq -r ._1)"
echo "TransceiverRegistration message $(message_summary "$(exercise_result "$set_peer_transaction" SetPeer | jq -c ._2)")"

echo "== 4. Locker locks $LOCK_AMOUNT CC with Transfer"
custodian_before_lock="$(amulet_total app-provider "$custodian_party")"
lock_registry="$(amulet_transfer_factory "$locker_party" "$custodian_party" "$LOCK_AMOUNT" "$locker_inputs")"
if [[ "$(jq -r .factoryId <<<"$lock_registry")" != "$transfer_factory_cid" ]]; then
  echo "the registry transfer factory changed after registration" >&2
  exit 1
fi
emitter_cid="$(current_emitter "$emitter_id" | jq -r .contractId)"
lock_transaction="$(submit_local app-user "ntt-lock-$run_label" "$(jq -n --arg party "$locker_party" '[$party]')" \
  "$(exercise_command "$NTT_MANAGER_TEMPLATE" "$manager_cid" Transfer "$(jq -n --arg user "$locker_party" \
    --arg emitter "$emitter_cid" --arg coreState "$core_state_cid" --arg ledger "$ledger_cid" --arg chain "$PEER_CHAIN_ID" \
    --arg recipient "$PEER_RECIPIENT_ADDRESS" --arg rawAmount "$LOCK_RAW_AMOUNT" --arg nonce "$OUTBOUND_NONCE" \
    --argjson inputs "$locker_inputs" --argjson extraArgs "$(registry_extra_args "$lock_registry")" '{
      user: $user, transceiverEmitterCid: $emitter, coreStateCid: $coreState, ledgerCid: $ledger,
      recipientChain: $chain, recipientAddress: $recipient, rawAmount: $rawAmount, nonce: $nonce, consistencyLevel: "0",
      inputHoldingCids: $inputs, feeAllocation: null, extraArgs: $extraArgs}')")" \
  "$(jq -c '. + $registry' --argjson registry "$(registry_disclosures "$lock_registry")" <<<"$(json_array \
    "$(operator_disclosure :Wormhole.Ntt.Manager:NttManager "$manager_cid")" \
    "$(operator_disclosure :Wormhole.Core.State:Emitter "$emitter_cid")" \
    "$(operator_disclosure :Wormhole.Core.State:CoreState "$core_state_cid")" \
    "$(operator_disclosure :Wormhole.Ntt.Ledger:LockedLedger "$ledger_cid")")")")"
lock_message="$(exercise_result "$lock_transaction" Transfer)"
echo "outbound NTT message $(message_summary "$lock_message") offset $(jq -r .offset <<<"$lock_transaction")"
ledger_after_lock="$(created_contract_argument "$lock_transaction" ":Wormhole.Ntt.Ledger:LockedLedger")"
ledger_cid="$(created_contract_id "$lock_transaction" ":Wormhole.Ntt.Ledger:LockedLedger")"
pot_cid="$(jq -er .custodyHoldingCid <<<"$ledger_after_lock")"
custodian_after_lock="$(amulet_total app-provider "$custodian_party")"
echo "LockedLedger balance $(jq -r .balance <<<"$ledger_after_lock"), custody pot ${pot_cid:0:16}"
echo "custodian CC $custodian_before_lock -> $custodian_after_lock"
if ! jq -ne --argjson balance "$(jq -r .balance <<<"$ledger_after_lock")" --argjson expected "$LOCK_AMOUNT" '$balance == $expected' >/dev/null; then
  echo "expected LockedLedger balance $LOCK_AMOUNT after the lock" >&2
  exit 1
fi

echo "== 5. Relayer on sv delivers the inbound transfer with Release"
recipient_before_release="$(amulet_total app-user "$recipient_party")"
release_vaa="$(ntt_transfer_vaa -sequence "$run_label" -emitter-chain "$PEER_CHAIN_ID" \
  -emitter-address "$PEER_TRANSCEIVER_ADDRESS" -source-manager "$PEER_MANAGER_ADDRESS" -recipient-manager "$manager_address" \
  -source-token "$PEER_TOKEN_ADDRESS" -recipient-party "$recipient_party" -ntt-message-id "$run_label" \
  -trimmed-decimals 8 -trimmed-amount "$RELEASE_TRIMMED_AMOUNT")"
replay_node_cid="$(covering_replay_node "$manager_namespace" "$(jq -r .digestHex <<<"$release_vaa")")"
release_registry="$(amulet_transfer_factory "$custodian_party" "$recipient_party" "$RELEASE_AMOUNT" "$(jq -n --arg pot "$pot_cid" '[$pot]')")"
echo "custodian-to-recipient kind $(jq -r .transferKind <<<"$release_registry")"
release_transaction="$(submit_local sv "ntt-release-$run_label" "$(jq -n --arg relayer "$relayer_party" '[$relayer]')" \
  "$(exercise_command "$NTT_MANAGER_TEMPLATE" "$manager_cid" Release "$(jq -n --arg relayer "$relayer_party" \
    --arg coreState "$core_state_cid" --arg replayNode "$replay_node_cid" --arg vaa "$(jq -r .vaaHex <<<"$release_vaa")" \
    --argjson keys "$(vaa_pub_keys_json "$release_vaa")" --arg recipient "$recipient_party" --arg ledger "$ledger_cid" \
    --argjson extraArgs "$(registry_extra_args "$release_registry")" '{
      executor: $relayer, coreStateCid: $coreState, replayNodeCid: $replayNode, vaaBytes: $vaa, pubKeys: $keys,
      recipient: $recipient, ledgerCid: $ledger, inputHoldingCids: [], extraArgs: $extraArgs}')")" \
  "$(jq -c '. + $registry' --argjson registry "$(registry_disclosures "$release_registry")" <<<"$(json_array \
    "$(operator_disclosure :Wormhole.Ntt.Manager:NttManager "$manager_cid")" \
    "$(operator_disclosure :Wormhole.Core.State:CoreState "$core_state_cid")" \
    "$(operator_disclosure :Wormhole.Core.Replay:ReplayNode "$replay_node_cid")" \
    "$(operator_disclosure :Wormhole.Ntt.Ledger:LockedLedger "$ledger_cid")" \
    "$(disclosed_contract app-provider "$custodian_party" :Splice.Amulet:Amulet "$pot_cid")")")")"
release_result="$(exercise_result "$release_transaction" Release)"
ledger_after_release="$(created_contract_argument "$release_transaction" ":Wormhole.Ntt.Ledger:LockedLedger")"
recipient_after_release="$(amulet_total app-user "$recipient_party")"
echo "Release decoded transfer $(jq -c '._2 | {decimals, amount, recipientChain}' <<<"$release_result")"
echo "LockedLedger balance $(jq -r .balance <<<"$ledger_after_release"), custody pot $(jq -r '.custodyHoldingCid // "none" | .[0:16]' <<<"$ledger_after_release")"
echo "recipient CC $recipient_before_release -> $recipient_after_release"
if ! jq -ne --argjson balance "$(jq -r .balance <<<"$ledger_after_release")" --argjson expected "$LEDGER_BALANCE_AFTER_RELEASE" '$balance == $expected' >/dev/null; then
  echo "expected LockedLedger balance $LEDGER_BALANCE_AFTER_RELEASE after the release" >&2
  exit 1
fi
if ! jq -ne --argjson before "$recipient_before_release" --argjson after "$recipient_after_release" --argjson amount "$RELEASE_AMOUNT" \
  '$after - $before == $amount' >/dev/null; then
  echo "expected the recipient to receive $RELEASE_AMOUNT CC" >&2
  exit 1
fi

record_ntt_json lockUnlock "$(jq -nc --arg custodian "$custodian_party" --arg manager "$manager_cid" \
  --arg managerAddress "$manager_address" --arg emitterId "$emitter_id" --arg emitterAddress "$emitter_address" \
  --arg recipient "$recipient_party" --arg relayer "$relayer_party" --arg lockOffset "$(jq -r .offset <<<"$lock_transaction")" \
  --arg releaseUpdateId "$(jq -r .updateId <<<"$release_transaction")" --argjson lockMessage "$lock_message" '{
    custodian: $custodian, managerCid: $manager, managerAddress: $managerAddress, emitterId: $emitterId,
    emitterAddress: $emitterAddress, recipient: $recipient, relayer: $relayer, lockOffset: $lockOffset,
    releaseUpdateId: $releaseUpdateId, lockMessage: $lockMessage}')"
echo "lock/unlock deployment recorded in $NTT_CONTRACTS"
