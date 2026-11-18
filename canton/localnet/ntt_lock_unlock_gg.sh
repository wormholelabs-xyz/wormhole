#!/usr/bin/env bash
# Register an NTT lock/unlock deployment of Canton Coin (Amulet) with guardianGovernance as the admin.
# The reserve custodian is then nttCustody. nttCustody gets an Amulet TransferPreapproval first.
# Usage: ntt_lock_unlock_gg.sh
# Precondition: NTT_PIN_VERSION=0.4.0, ntt_governance.sh ran, custody-artifacts.json names nttCustody.
# Output: the "lockUnlockGg" entry of $LOCALNET_STATE_DIR/ntt-contracts.json.
set -euo pipefail
# shellcheck source=ntt_lib.sh
source "$(dirname "${BASH_SOURCE[0]}")/ntt_lib.sh"

readonly PEER_CHAIN_ID=2
readonly PEER_MANAGER_ADDRESS="000000000000000000000000e8e8e8e8e8e8e8e8e8e8e8e8e8e8e8e8e8e8e8e8"
readonly PEER_TRANSCEIVER_ADDRESS="000000000000000000000000d9d9d9d9d9d9d9d9d9d9d9d9d9d9d9d9d9d9d9d9"
readonly AMULET_DECIMALS=10
readonly PROVIDER_VALIDATOR="app-provider"
readonly PROVIDER_WALLET_USER="app-provider"
readonly PROVIDER_TAP_AMOUNT_USD="10.0"
readonly TRANSFER_PREAPPROVAL_PROPOSAL_TEMPLATE="#splice-wallet:Splice.Wallet.TransferPreapproval:TransferPreapprovalProposal"
readonly PREAPPROVAL_WAIT_MAX_POLLS=60
readonly PREAPPROVAL_WAIT_POLL_SECONDS=2

[[ -n "$NTT_CUSTODY_PARTY" ]] || fail "set NTT_PIN_VERSION=0.4.0: ntt $NTT_VERSION has no nttCustody"
run_label="$(date +%s)"
gg_only="$(jq -n --arg gg "$GG_PARTY" '[$gg]')"
dso="$(dso_party)"

# custody_amulet_preapprovals: the Amulet TransferPreapproval contracts with receiver nttCustody.
custody_amulet_preapprovals() {
  active_contracts "$PROVIDER_VALIDATOR" "$NTT_CUSTODY_PARTY" ":Splice.AmuletRules:TransferPreapproval" \
    | jq -c --arg custody "$NTT_CUSTODY_PARTY" '[.[] | select(.createArgument.receiver == $custody)]'
}

echo "== 1. Amulet TransferPreapproval of nttCustody"
if [[ "$(custody_amulet_preapprovals | jq length)" -eq 0 ]]; then
  provider_party="$(curl_bounded -sSf "$(validator_api_url "$PROVIDER_VALIDATOR")/validator-user" \
    -H "@$(subject_auth_header_file "$PROVIDER_WALLET_USER")" | jq -er .party_id)"
  wallet_tap "$PROVIDER_VALIDATOR" "$PROVIDER_WALLET_USER" "$PROVIDER_TAP_AMOUNT_USD"
  proposal_transaction="$(submit_interactive "ntt-custody-preapproval-proposal-$run_label" \
    "$(jq -n --arg custody "$NTT_CUSTODY_PARTY" '[$custody]')" \
    "$(create_command "$TRANSFER_PREAPPROVAL_PROPOSAL_TEMPLATE" "$(jq -n --arg custody "$NTT_CUSTODY_PARTY" \
      --arg provider "$provider_party" --arg dso "$dso" '{receiver: $custody, provider: $provider, expectedDso: $dso}')")" \
    "$(ntt_custody_signers_json)")"
  echo "TransferPreapprovalProposal $(created_contract_id "$proposal_transaction" ":Splice.Wallet.TransferPreapproval:TransferPreapprovalProposal" | cut -c1-16), provider $provider_party"
  for (( poll = 1; poll <= PREAPPROVAL_WAIT_MAX_POLLS; poll++ )); do
    [[ "$(custody_amulet_preapprovals | jq length)" -eq 0 ]] || break
    sleep "$PREAPPROVAL_WAIT_POLL_SECONDS"
  done
fi
custody_preapprovals="$(custody_amulet_preapprovals)"
[[ "$(jq length <<<"$custody_preapprovals")" -eq 1 ]] \
  || fail "expected one Amulet TransferPreapproval of nttCustody, found $(jq length <<<"$custody_preapprovals")"
echo "nttCustody TransferPreapproval $(jq -r '.[0].contractId[0:16]' <<<"$custody_preapprovals")"

echo "== 2. RegisterManager (LockUnlock, admin guardianGovernance): ${NTT_GG_SIGNING_OWNERS[*]}"
transfer_factory_cid="$(amulet_transfer_factory "$NTT_CUSTODY_PARTY" "$NTT_CUSTODY_PARTY" "1.0" '[]' | jq -er .factoryId)"
governance_cid="$(current_ntt_governance)"
emitter_registry_cid="$(current_emitter_registry)"
replay_root_registry_cid="$(core_contract_id replayRootRegistry)"
core_state_cid="$(core_contract_id coreState)"
amulet_instrument="$(jq -n --arg dso "$dso" '{admin: $dso, id: "Amulet"}')"
register_transaction="$(submit_interactive "ntt-register-lock-unlock-gg-$run_label" "$gg_only" \
  "$(exercise_command "$NTT_GOVERNANCE_TEMPLATE" "$governance_cid" RegisterManager "$(jq -n \
    --arg admin "$GG_PARTY" --arg emitterRegistry "$emitter_registry_cid" --arg replayRootRegistry "$replay_root_registry_cid" \
    --argjson instrument "$amulet_instrument" --arg factory "$transfer_factory_cid" --arg coreState "$core_state_cid" \
    --arg decimals "$AMULET_DECIMALS" '{
      admin: $admin, chainId: "75", emitterRegistryCid: $emitterRegistry, replayRootRegistryCid: $replayRootRegistry,
      instrumentId: $instrument, instrumentNonce: "0", tokenConfig: "LockUnlock", tokenDecimals: $decimals,
      factory: {tag: "TransferFactoryCid", value: $factory}, coreStateCid: $coreState, consistencyLevel: "0",
      emitterFeeAllocation: null, claimFeeAllocation: null, feeAllocation: null}')")" \
  "$(gg_ceremony_signers_json)" \
  "$(json_array \
    "$(operator_disclosure :Wormhole.Core.State:EmitterRegistry "$emitter_registry_cid")" \
    "$(operator_disclosure :Wormhole.Core.State:ReplayRootRegistry "$replay_root_registry_cid")" \
    "$(operator_disclosure :Wormhole.Core.State:CoreState "$core_state_cid")")")"
register_result="$(exercise_result "$register_transaction" :Wormhole.Ntt.Governance:NttGovernance RegisterManager)"
manager_cid="$(jq -r ._2 <<<"$register_result")"
manager_json="$(created_contract_argument "$register_transaction" ":Wormhole.Ntt.Manager:NttManager")"
manager_address="$(jq -r .managerAddress <<<"$manager_json")"
emitter_id="$(jq -r .transceiverEmitterId <<<"$manager_json")"
[[ "$(jq -r .admin <<<"$manager_json")" == "$GG_PARTY" ]] || fail "manager admin is not guardianGovernance"
[[ "$(jq -r .reserveCustodian <<<"$manager_json")" == "$NTT_CUSTODY_PARTY" ]] || fail "manager reserveCustodian is not nttCustody"
echo "manager ${manager_cid:0:16} address $manager_address emitterId $emitter_id reserveCustodian nttCustody"

echo "== 3. SetPeer chain $PEER_CHAIN_ID (guardianGovernance)"
emitter_cid="$(current_emitter "$emitter_id" | jq -r .contractId)"
set_peer_transaction="$(submit_interactive "ntt-lock-unlock-gg-set-peer-$run_label" "$gg_only" \
  "$(exercise_command "$NTT_MANAGER_TEMPLATE" "$manager_cid" SetPeer "$(jq -n --arg chain "$PEER_CHAIN_ID" \
    --arg manager "$PEER_MANAGER_ADDRESS" --arg transceiver "$PEER_TRANSCEIVER_ADDRESS" --arg emitter "$emitter_cid" \
    --arg coreState "$core_state_cid" --arg gg "$GG_PARTY" '{
      chainId: $chain, peer: {managerAddress: $manager, transceiverAddress: $transceiver, decimals: "8"},
      transceiverEmitterCid: $emitter, coreStateCid: $coreState, consistencyLevel: "0", payer: $gg, feeAllocation: null}')")" \
  "$(gg_ceremony_signers_json)" \
  "$(json_array "$(operator_disclosure :Wormhole.Core.State:Emitter "$emitter_cid")" \
    "$(operator_disclosure :Wormhole.Core.State:CoreState "$core_state_cid")")")"
manager_cid="$(exercise_result "$set_peer_transaction" :Wormhole.Ntt.Manager:NttManager SetPeer | jq -r ._1)"

record_ntt_json lockUnlockGg "$(jq -nc --arg manager "$manager_cid" --arg managerAddress "$manager_address" \
  --arg emitterId "$emitter_id" --arg custody "$NTT_CUSTODY_PARTY" \
  --arg peerManager "$PEER_MANAGER_ADDRESS" --arg peerTransceiver "$PEER_TRANSCEIVER_ADDRESS" '{
    managerCid: $manager, managerAddress: $managerAddress, emitterId: $emitterId, reserveCustodian: $custody,
    peerChainId: 2, peerManagerAddress: $peerManager, peerTransceiverAddress: $peerTransceiver}')"
echo "lock/unlock deployment with admin guardianGovernance recorded in $NTT_CONTRACTS"
