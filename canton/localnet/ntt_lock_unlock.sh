#!/usr/bin/env bash
# Run an NTT lock/unlock deployment of Canton Coin (Amulet) on LocalNet. The deployment admin is the custodian.
# The "== N." lines of the output name the steps. README.md "NTT end-to-end run" describes them.
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
# NTT_RELEASE_AFTER_ROUNDS > 0 releases from a custody pot that is that many mining rounds old. Token-standard
# transfers use the round of ExternalPartyConfigState, which the DSO updates each 24 hours; a pot from round 0
# makes the option have no effect.
if ! check_release_after_rounds "${NTT_RELEASE_AFTER_ROUNDS:-0}"; then
  echo "NTT_RELEASE_AFTER_ROUNDS must be a decimal integer from 0 to $MAX_RELEASE_AFTER_ROUNDS, got '${NTT_RELEASE_AFTER_ROUNDS:-}'" >&2
  exit 1
fi
readonly RELEASE_AFTER_ROUNDS=$((10#${NTT_RELEASE_AFTER_ROUNDS:-0}))
readonly RELEASE_WAIT_POLL_SECONDS=30
readonly RELEASE_WAIT_MAX_POLLS=$(( (RELEASE_AFTER_ROUNDS + 2) * 24 ))

run_label="$(date +%s)"

# amulet_total_units <validator> <party>: the unlocked Amulet of the party, in units of 10^-10 CC.
amulet_total_units() {
  local amulet_amounts
  amulet_amounts="$(amulet_holdings "$1" "$2" | jq -c 'map(.amount)')"
  if [[ "$amulet_amounts" == "[]" ]]; then
    echo 0
    return 0
  fi
  json_decimal_array_total_units "$AMULET_DECIMALS" "$amulet_amounts"
}

# ledger_balance_units <locked-ledger-create-argument-json>
ledger_balance_units() {
  decimal_to_units "$(jq -er .balance <<<"$1")" "$AMULET_DECIMALS"
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
register_result="$(exercise_result "$register_transaction" :Wormhole.Ntt.Governance:NttGovernance RegisterManager)"
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
manager_cid="$(exercise_result "$set_peer_transaction" :Wormhole.Ntt.Manager:NttManager SetPeer | jq -r ._1)"
echo "TransceiverRegistration message $(message_summary "$(exercise_result "$set_peer_transaction" :Wormhole.Ntt.Manager:NttManager SetPeer | jq -c ._2)")"

echo "== 4. Locker locks $LOCK_AMOUNT CC with Transfer"
custodian_before_lock_units="$(amulet_total_units app-provider "$custodian_party")"
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
lock_message="$(exercise_result "$lock_transaction" :Wormhole.Ntt.Manager:NttManager Transfer)"
echo "outbound NTT message $(message_summary "$lock_message") update $(jq -r .updateId <<<"$lock_transaction")"
ledger_after_lock="$(created_contract_argument "$lock_transaction" ":Wormhole.Ntt.Ledger:LockedLedger")"
ledger_cid="$(created_contract_id "$lock_transaction" ":Wormhole.Ntt.Ledger:LockedLedger")"
pot_cid="$(jq -er .custodyHoldingCid <<<"$ledger_after_lock")"
custodian_after_lock_units="$(amulet_total_units app-provider "$custodian_party")"
echo "LockedLedger balance $(jq -r .balance <<<"$ledger_after_lock"), custody pot ${pot_cid:0:16}"
echo "custodian units $custodian_before_lock_units -> $custodian_after_lock_units"
if [[ "$(ledger_balance_units "$ledger_after_lock")" != "$(decimal_to_units "$LOCK_AMOUNT" "$AMULET_DECIMALS")" ]]; then
  echo "expected LockedLedger balance $LOCK_AMOUNT after the lock" >&2
  exit 1
fi

pot_created_round="$(amulet_holdings_with_round app-provider "$custodian_party" \
  | jq -er --arg pot "$pot_cid" '.[] | select(.contractId == $pot) | .createdRound')"
echo "custody pot created in round $pot_created_round"
if (( RELEASE_AFTER_ROUNDS > 0 )); then
  release_round=$(( pot_created_round + RELEASE_AFTER_ROUNDS ))
  echo "== 4a. Wait until open round $release_round (at most $RELEASE_WAIT_MAX_POLLS polls of ${RELEASE_WAIT_POLL_SECONDS}s)"
  for (( poll = 1; poll <= RELEASE_WAIT_MAX_POLLS; poll++ )); do
    open_round="$(latest_open_round)"
    if (( open_round >= release_round )); then
      break
    fi
    sleep "$RELEASE_WAIT_POLL_SECONDS"
  done
  if (( open_round < release_round )); then
    echo "open round $open_round did not reach $release_round" >&2
    exit 1
  fi
  echo "latest open round $open_round"
fi

echo "== 5. Relayer on sv delivers the inbound transfer with Release"
recipient_before_release_units="$(amulet_total_units app-user "$recipient_party")"
custodian_before_release="$(amulet_holdings_with_round app-provider "$custodian_party")"
release_vaa="$(ntt_transfer_vaa -sequence "$run_label" -emitter-chain "$PEER_CHAIN_ID" \
  -emitter-address "$PEER_TRANSCEIVER_ADDRESS" -source-manager "$PEER_MANAGER_ADDRESS" -recipient-manager "$manager_address" \
  -source-token "$PEER_TOKEN_ADDRESS" -recipient-party "$recipient_party" -ntt-message-id "$run_label" \
  -trimmed-decimals 8 -trimmed-amount "$RELEASE_TRIMMED_AMOUNT")"
replay_node_cid="$(covering_replay_node "$manager_namespace" "$(jq -r .digestHex <<<"$release_vaa")")"
release_registry="$(amulet_transfer_factory "$custodian_party" "$recipient_party" "$RELEASE_AMOUNT" "$(jq -n --arg pot "$pot_cid" '[$pot]')")"
echo "custodian-to-recipient kind $(jq -r .transferKind <<<"$release_registry")"
# release_command <vaa-json> <replay-node-cid> <ledger-cid> <registry-json>
release_command() {
  exercise_command "$NTT_MANAGER_TEMPLATE" "$manager_cid" Release "$(jq -n --arg relayer "$relayer_party" \
    --arg coreState "$core_state_cid" --arg replayNode "$2" --arg vaa "$(jq -r .vaaHex <<<"$1")" \
    --argjson keys "$(vaa_pub_keys_json "$1")" --arg recipient "$recipient_party" --arg ledger "$3" \
    --argjson extraArgs "$(registry_extra_args "$4")" '{
      executor: $relayer, coreStateCid: $coreState, replayNodeCid: $replayNode, vaaBytes: $vaa, pubKeys: $keys,
      recipient: $recipient, ledgerCid: $ledger, inputHoldingCids: [], extraArgs: $extraArgs}')"
}
# release_disclosures <replay-node-cid> <ledger-cid> <custody-pot-cid> <registry-json>
release_disclosures() {
  jq -c '. + $registry' --argjson registry "$(registry_disclosures "$4")" <<<"$(json_array \
    "$(operator_disclosure :Wormhole.Ntt.Manager:NttManager "$manager_cid")" \
    "$(operator_disclosure :Wormhole.Core.State:CoreState "$core_state_cid")" \
    "$(operator_disclosure :Wormhole.Core.Replay:ReplayNode "$1")" \
    "$(operator_disclosure :Wormhole.Ntt.Ledger:LockedLedger "$2")" \
    "$(disclosed_contract app-provider "$custodian_party" :Splice.Amulet:Amulet "$3")")"
}
relayer_only="$(jq -n --arg relayer "$relayer_party" '[$relayer]')"
release_transaction="$(submit_local sv "ntt-release-$run_label" "$relayer_only" \
  "$(release_command "$release_vaa" "$replay_node_cid" "$ledger_cid" "$release_registry")" \
  "$(release_disclosures "$replay_node_cid" "$ledger_cid" "$pot_cid" "$release_registry")")"
release_result="$(exercise_result "$release_transaction" :Wormhole.Ntt.Manager:NttManager Release)"
ledger_after_release="$(created_contract_argument "$release_transaction" ":Wormhole.Ntt.Ledger:LockedLedger")"
recipient_after_release_units="$(amulet_total_units app-user "$recipient_party")"
echo "Release decoded transfer $(jq -c '._2 | {decimals, amount, recipientChain}' <<<"$release_result")"
echo "LockedLedger balance $(jq -r .balance <<<"$ledger_after_release"), custody pot $(jq -r '.custodyHoldingCid // "none" | .[0:16]' <<<"$ledger_after_release")"
echo "recipient units $recipient_before_release_units -> $recipient_after_release_units"
custodian_after_release="$(amulet_holdings_with_round app-provider "$custodian_party")"
echo "custodian Amulets before release $custodian_before_release"
echo "custodian Amulets after release  $custodian_after_release"
if [[ "$(ledger_balance_units "$ledger_after_release")" != "$(decimal_to_units "$LEDGER_BALANCE_AFTER_RELEASE" "$AMULET_DECIMALS")" ]]; then
  echo "expected LockedLedger balance $LEDGER_BALANCE_AFTER_RELEASE after the release" >&2
  exit 1
fi
if (( recipient_after_release_units - recipient_before_release_units != $(decimal_to_units "$RELEASE_AMOUNT" "$AMULET_DECIMALS") )); then
  echo "expected the recipient to receive $RELEASE_AMOUNT CC" >&2
  exit 1
fi

echo "== 5a. The same Release VAA again is rejected"
ledger_cid="$(created_contract_id "$release_transaction" ":Wormhole.Ntt.Ledger:LockedLedger")"
pot_cid="$(jq -er .custodyHoldingCid <<<"$ledger_after_release")"
replay_node_cid="$(covering_replay_node "$manager_namespace" "$(jq -r .digestHex <<<"$release_vaa")")"
replay_registry="$(amulet_transfer_factory "$custodian_party" "$recipient_party" "$RELEASE_AMOUNT" "$(jq -n --arg pot "$pot_cid" '[$pot]')")"
submit_local_expect_rejection sv "ntt-release-replay-$run_label" "$relayer_only" \
  "$(release_command "$release_vaa" "$replay_node_cid" "$ledger_cid" "$replay_registry")" \
  "$(release_disclosures "$replay_node_cid" "$ledger_cid" "$pot_cid" "$replay_registry")" "digest already consumed"

record_ntt_json lockUnlock "$(jq -nc --arg custodian "$custodian_party" --arg manager "$manager_cid" \
  --arg managerAddress "$manager_address" --arg emitterId "$emitter_id" --arg emitterAddress "$emitter_address" \
  --arg recipient "$recipient_party" --arg relayer "$relayer_party" --arg lockUpdateId "$(jq -r .updateId <<<"$lock_transaction")" \
  --arg releaseUpdateId "$(jq -r .updateId <<<"$release_transaction")" --argjson lockMessage "$lock_message" '{
    custodian: $custodian, managerCid: $manager, managerAddress: $managerAddress, emitterId: $emitterId,
    emitterAddress: $emitterAddress, recipient: $recipient, relayer: $relayer, lockUpdateId: $lockUpdateId,
    releaseUpdateId: $releaseUpdateId, lockMessage: $lockMessage}')"
echo "lock/unlock deployment recorded in $NTT_CONTRACTS"
