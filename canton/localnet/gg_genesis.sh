#!/usr/bin/env bash
# Genesis on LocalNet. LabsOperator creates one WormholeGenesisOffer on app-provider. guardianGovernance, feeCustody and
# nttCustody accept it once (AcceptGenesis, 2-of-3 owner signatures each). That one transaction creates the CoreState,
# the EmitterRegistry, the ReplayRootRegistry, the NttGovernance root and the gg CoinFactory. gg is a signatory of
# each; LabsOperator is the onboarding fee admin of the two registries.
# Before the acceptance, the guardian side compares the offer that sv shows with its own agreed parameters.
# These acceptances must fail first:
#   guardianGovernance with 1 of 3 owner signatures; nttCustody without signatures; agreed parameters that differ from
#   the offer (Daml check of AcceptGenesis).
# Rerun after a failure: the script reuses an active offer with the agreed parameters. When the roots exist, it only
# checks the CoreState.
# Usage: [GENESIS_GUARDIAN_SET_INDEX=<index>] gg_genesis.sh (default index 0)
# Then create the user wormhole-watcher with only CanReadAs(guardianGovernance) on each guardian host, for the
# emitter id scan.
# Precondition: gg_host.sh ran; custody_host.sh ran for feeCustody and nttCustody; ntt_upload_dars.sh ran.
# Output: $LOCALNET_STATE_DIR/core-contracts.json.
set -euo pipefail
# shellcheck source=gg_lib.sh
source "$(dirname "${BASH_SOURCE[0]}")/gg_lib.sh"
# shellcheck source=watcher_harness_lib.sh
source "$LOCALNET_SCRIPT_DIR/watcher_harness_lib.sh"

readonly DEVNET_GUARDIAN_ADDRESS="befa429d57cd18b7f8a4d91a2da9ab4af05d0fbe"
readonly DEVNET_GUARDIAN_PUBLIC_KEY="04d4a4629979f0c9fa0f0bb54edf33f87c8c5a1f42c0350a30d68f7e967023e34e495a8ebf5101036d0fd66e3b0a8c7c61b65fceeaf487ab3cd1b5b7b50beb7970"
readonly GENESIS_GUARDIAN_SET_INDEX="${GENESIS_GUARDIAN_SET_INDEX:-0}"
readonly LABS_OPERATOR_VALIDATOR="app-provider"
# sv sees the offer some time after app-provider commits it.
readonly OFFER_VISIBLE_MAX_POLLS=30

[[ -n "$FEE_CUSTODY_PARTY" ]] || fail "missing $FEE_CUSTODY_ARTIFACTS. Run CUSTODY_PARTY_NAME=feeCustody custody_host.sh first."
[[ -n "$CUSTODY_PARTY" ]] || fail "missing $CUSTODY_ARTIFACTS. Run custody_host.sh for nttCustody first."
check_fee_custody_is_in_the_guardian_namespace "$FEE_CUSTODY_PARTY" "$GG_PARTY" "$CUSTODY_PARTY" || exit 1
[[ "${CUSTODY_PARTY%%::*}" == "nttCustody" && "${CUSTODY_PARTY#*::}" == "${GG_PARTY#*::}" ]] \
  || fail "$CUSTODY_PARTY is not nttCustody in the namespace of $GG_PARTY"

dso_party="$(curl_bounded -sSf "$(json_api_url sv)/v2/parties?pageSize=1000" -H "@$ADMIN_AUTH_HEADER" \
  | jq -er '[.partyDetails[].party | select(startswith("DSO::"))] | if length == 1 then .[0] else error("expected one DSO party") end')"
# The guardian side builds its agreed parameters from its own inputs.
agreed_parameters="$(first_genesis_parameters "$GG_PARTY" "$FEE_CUSTODY_PARTY" "$CUSTODY_PARTY" "$LABS_OPERATOR_PARTY" \
  "$dso_party" Amulet "$GENESIS_GUARDIAN_SET_INDEX" "$DEVNET_GUARDIAN_ADDRESS:$DEVNET_GUARDIAN_PUBLIC_KEY")"
genesis_parties="$(jq -cn --arg gg "$GG_PARTY" --arg fee "$FEE_CUSTODY_PARTY" --arg ntt "$CUSTODY_PARTY" '[$gg, $fee, $ntt]')"

# genesis_signers_json <gg owner names> <feeCustody owner names> <nttCustody owner names>: comma-separated owner names.
genesis_signers_json() {
  local -a gg_owners fee_custody_owners ntt_custody_owners
  IFS=, read -r -a gg_owners <<<"$1"
  IFS=, read -r -a fee_custody_owners <<<"$2"
  IFS=, read -r -a ntt_custody_owners <<<"$3"
  jq -s 'add' \
    <(if [[ -n "$1" ]]; then gg_signers_json "${gg_owners[@]}"; else echo '{}'; fi) \
    <(if [[ -n "$2" ]]; then namespace_party_signers_json "$FEE_CUSTODY_PARTY" "${fee_custody_owners[@]}"; else echo '{}'; fi) \
    <(if [[ -n "$3" ]]; then namespace_party_signers_json "$CUSTODY_PARTY" "${ntt_custody_owners[@]}"; else echo '{}'; fi)
}

active_core_states="$(active_contracts "$SUBMITTING_VALIDATOR" "$GG_PARTY" :Wormhole.Core.State:CoreState \
  | jq -c --arg gg "$GG_PARTY" '[.[] | select(.createArgument.guardianGovernance == $gg)]')"
case "$(jq length <<<"$active_core_states")" in
  0) ;;
  1)
    echo "CoreState exists; check it and skip the genesis"
    check_core_state_pays_fees_to_fee_custody "$(jq -c '.[0]' <<<"$active_core_states")" "$GG_PARTY" "$FEE_CUSTODY_PARTY" || exit 1
    for validator_name in "${GUARDIAN_HOSTS[@]}"; do
      ensure_watcher_user "$validator_name"
    done
    exit 0
    ;;
  *) fail "found $(jq length <<<"$active_core_states") CoreState contracts of guardianGovernance; expected at most one" ;;
esac

# active_offers: the active WormholeGenesisOffer contracts of LabsOperator that guardianGovernance sees on sv.
active_offers() {
  active_contracts "$SUBMITTING_VALIDATOR" "$GG_PARTY" :Wormhole.Genesis:WormholeGenesisOffer \
    | jq -c --arg operator "$LABS_OPERATOR_PARTY" '[.[] | select(.createArgument.operator == $operator)]'
}
if [[ "$(active_offers | jq length)" == 0 ]]; then
  echo "LabsOperator creates the WormholeGenesisOffer on $LABS_OPERATOR_VALIDATOR (local party, no gg signature)"
  offer_transaction="$(submit_local "$LABS_OPERATOR_VALIDATOR" "genesis-offer-$(date +%s)" "$(jq -cn --arg party "$LABS_OPERATOR_PARTY" '[$party]')" \
    "$(create_command "$GENESIS_OFFER_TEMPLATE" "$(genesis_offer_arguments "$LABS_OPERATOR_PARTY" "$agreed_parameters")")")"
  echo "WormholeGenesisOffer $(created_contract_id "$offer_transaction" ":Wormhole.Genesis:WormholeGenesisOffer") (update $(jq -r .updateId <<<"$offer_transaction"))"
fi
for (( poll = 1; poll <= OFFER_VISIBLE_MAX_POLLS; poll++ )); do
  [[ "$(active_offers | jq length)" == 0 ]] || break
  sleep 1
done
offer_event="$(active_offers | jq -ec 'if length == 1 then .[0] else error("expected one active WormholeGenesisOffer of LabsOperator, found \(length)") end')"
offer_cid="$(jq -r .contractId <<<"$offer_event")"
echo "Guardian side: compare the offer ${offer_cid:0:16} on $SUBMITTING_VALIDATOR with the agreed parameters"
accept_arguments="$(accept_genesis_arguments "$(jq -c .createArgument <<<"$offer_event")" "$agreed_parameters")"
echo "PASS: the offer carries exactly the agreed parameters"
accept_command="$(exercise_command "$GENESIS_OFFER_TEMPLATE" "$offer_cid" AcceptGenesis "$accept_arguments")"

# expect_acceptance_rejected <check-name> <command-json> <signers-json> <expected-text>...: AcceptGenesis fails with each
# expected text.
expect_acceptance_rejected() {
  local check_name="$1" command_json="$2" signers_json="$3" expected_fragment
  shift 3
  echo "Negative check: $check_name"
  if submit_guardian_ceremony "genesis-accept-$check_name-$(date +%s)" "$genesis_parties" "$command_json" "$signers_json" \
      > "$gg_work_dir/genesis-$check_name.out" 2> "$gg_work_dir/genesis-$check_name.err"; then
    fail "AcceptGenesis committed: $check_name"
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
expect_acceptance_rejected gg-one-owner "$accept_command" "$(genesis_signers_json owner1 owner1,owner2 owner1,owner2)" \
  FAILED_TO_EXECUTE_TRANSACTION \
  "Received 1 valid signatures from distinct keys (0 invalid), but expected at least 2 valid for guardianGovernance::"
expect_acceptance_rejected ntt-custody-unsigned "$accept_command" "$(genesis_signers_json owner1,owner2 owner1,owner2 "")" \
  "did not provide an external signature: $CUSTODY_PARTY"
expect_acceptance_rejected agreed-parameters-differ \
  "$(exercise_command "$GENESIS_OFFER_TEMPLATE" "$offer_cid" AcceptGenesis \
    "$(jq -c '.agreedParameters.genesisGuardianSet.guardianSetIndex |= (tonumber + 1 | tostring)' <<<"$accept_arguments")")" \
  "$(genesis_signers_json owner1,owner2 owner1,owner2 owner1,owner2)" \
  "genesis: agreed parameters differ from the offer"

echo "AcceptGenesis: guardianGovernance, feeCustody and nttCustody, owner1 and owner2 (2-of-3 each)"
accept_command_id="genesis-accept-$(date +%s)"
accept_transaction="$(submit_guardian_ceremony "$accept_command_id" "$genesis_parties" "$accept_command" \
  "$(genesis_signers_json owner1,owner2 owner1,owner2 owner1,owner2)")"
jq -e --arg cid "$offer_cid" '[.events[] | .ExercisedEvent? // empty | select(.contractId == $cid and .choice == "AcceptGenesis" and .consuming)] | length == 1' \
  <<<"$accept_transaction" > /dev/null || fail "AcceptGenesis did not consume the offer $offer_cid"
created_core_state_event="$(jq -ce '[.events[] | .CreatedEvent? // empty | select(.templateId | endswith(":Wormhole.Core.State:CoreState"))]
  | if length == 1 then .[0] else error("expected one created CoreState, found \(length)") end' <<<"$accept_transaction")"
check_core_state_pays_fees_to_fee_custody "$created_core_state_event" "$GG_PARTY" "$FEE_CUSTODY_PARTY" || exit 1
if jq -e --arg operator "$LABS_OPERATOR_PARTY" \
    '[.events[] | .CreatedEvent? // empty | (.signatories[], (.observers // [])[])] | index($operator)' <<<"$accept_transaction" > /dev/null; then
  fail "a contract that AcceptGenesis created names LabsOperator as a stakeholder"
fi
jq -n --arg coreState "$(created_contract_id "$accept_transaction" ":Wormhole.Core.State:CoreState")" \
  --arg emitterRegistry "$(created_contract_id "$accept_transaction" ":Wormhole.Core.State:EmitterRegistry")" \
  --arg replayRootRegistry "$(created_contract_id "$accept_transaction" ":Wormhole.Core.State:ReplayRootRegistry")" \
  --arg nttGovernance "$(created_contract_id "$accept_transaction" ":Wormhole.Ntt.Governance:NttGovernance")" \
  --arg coinFactory "$(created_contract_id "$accept_transaction" ":Token.CIP0056.CoinFactory:CoinFactory")" \
  --arg updateId "$(jq -r .updateId <<<"$accept_transaction")" --arg commandId "$accept_command_id" \
  '{coreState: $coreState, emitterRegistry: $emitterRegistry, replayRootRegistry: $replayRootRegistry,
    nttGovernance: $nttGovernance, coinFactory: $coinFactory, genesisUpdateId: $updateId, genesisCommandId: $commandId}' \
  > "$CORE_CONTRACTS"
echo "Genesis roots (update $(jq -r .updateId <<<"$accept_transaction")):"
jq . "$CORE_CONTRACTS"

for validator_name in "${GUARDIAN_HOSTS[@]}"; do
  ensure_watcher_user "$validator_name"
done
