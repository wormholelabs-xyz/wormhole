#!/usr/bin/env bash
# Operator independence rehearsal. The operator deploys and offers genesis alone, the guardians accept once, and then
# each Wormhole function runs with app-provider, the participant of the operator, disconnected.
#   Phase 1, setup by the operator alone:
#     1   LabsOperator is a local party of app-provider. The DARs (core, token-cip0056, ntt, wormhole-genesis) are vetted.
#     2   Each guardian participant vets the same DARs. The gg ceremony count stays the same.
#     3-4 gg_genesis.sh: LabsOperator offers genesis; gg, feeCustody and nttCustody accept once.
#   Phase 2, app-provider disconnected. Local parties submit on sv or app-user, with disclosures read as gg on sv:
#     5 disconnect app-provider    6 RegisterEmitter        7 PublishMessage at fee 0   8 watcher on sv observes it
#     9 ClaimReplayRoot           10 VerifyAndConsumeVAA   11 the same VAA again fails  12 relay SetMessageFee 0
#    13 relay TransferFees        14 NTT RegisterManager   15 SetPeer                   16 Transfer (lock), observed
#    17 Release                   18 relay GuardianSetUpgrade                           19 SetRegistrationFee cannot run
#   Phase 3, controls:
#    20 Reconnect app-provider, disconnect sv and app-user. A publish times out; the unresponsive parties are exactly
#       guardianGovernance and feeCustody.
#    21 gg-submitted transactions since step 1: (a) the ceremony log of gg_lib.sh, (b) transactions with a command id in
#       the gg stream on sv. Each is exactly 1.
# Usage: gg_operator_independence.sh [<current-set-key-indices> [<new-set-key-indices>]]   (default 0 1)
# Precondition: reset LocalNet; gg_host.sh ran; custody_host.sh ran for feeCustody and nttCustody; NTT_REPO_DIR has the
# pinned ntt and wormhole-genesis DARs. The EXIT trap stops the watcher harness and reconnects each participant.
set -euo pipefail
# shellcheck source=ntt_lib.sh
source "$(dirname "${BASH_SOURCE[0]}")/ntt_lib.sh"
readonly WATCHER_DIR="$LOCALNET_STATE_DIR/operator-independence-watcher"
# shellcheck source=watcher_harness_lib.sh
source "$LOCALNET_SCRIPT_DIR/watcher_harness_lib.sh"

readonly CONTROL_TICKER_SECONDS=600
readonly OBSERVATION_WAIT_SECONDS=60
readonly REPLAY_NAMESPACE_PREFIX="operator-independence"
readonly REPLAY_SPLIT_THRESHOLD=64
readonly MESSAGE_PAYLOAD_HEX="6f70657261746f72206f66666c696e65"
readonly TRANSFER_FEES_RECIPIENT="000000000000000000000000f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0"
readonly TRANSFER_FEES_AMOUNT=1
readonly PEER_CHAIN_ID=2
readonly PEER_MANAGER_ADDRESS="000000000000000000000000e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1"
readonly PEER_TRANSCEIVER_ADDRESS="000000000000000000000000d2d2d2d2d2d2d2d2d2d2d2d2d2d2d2d2d2d2d2d2"
readonly PEER_TOKEN_ADDRESS="000000000000000000000000c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3"
readonly PEER_RECIPIENT_ADDRESS="000000000000000000000000b4b4b4b4b4b4b4b4b4b4b4b4b4b4b4b4b4b4b4b4"
readonly AMULET_DECIMALS=10
readonly LOCK_AMOUNT="5.0"
readonly LOCK_RAW_AMOUNT=50000000000
readonly RELEASE_AMOUNT="2.0"
readonly RELEASE_TRIMMED_AMOUNT=200000000
readonly LEDGER_BALANCE_AFTER_RELEASE="3.0"
readonly TAP_AMOUNT_USD="10.0"
readonly OUTBOUND_NONCE=9
readonly LABS_OPERATOR_VALIDATOR="app-provider"
readonly NTT_WALLET_VALIDATOR="app-user"

current_set_key_indices="${1:-0}"
new_set_key_indices="${2:-1}"
for key_indices in "$current_set_key_indices" "$new_set_key_indices"; do
  [[ "$key_indices" =~ ^[0-9]+(,[0-9]+)*$ ]] || { echo "key indices must be comma-separated integers: $key_indices" >&2; exit 2; }
done
[[ "$current_set_key_indices" != "$new_set_key_indices" ]] || { echo "the new set must differ from the current set" >&2; exit 2; }
[[ -n "$CUSTODY_PARTY" ]] || fail "missing $CUSTODY_ARTIFACTS. Run custody_host.sh for nttCustody first."
[[ -n "$FEE_CUSTODY_PARTY" ]] || fail "missing $FEE_CUSTODY_ARTIFACTS. Run CUSTODY_PARTY_NAME=feeCustody custody_host.sh first."
run_label="$(date +%s)"
mkdir -p "$WATCHER_DIR"
clock_ticker_pid=""

restore_connections() {
  local exit_status=$?
  stop_clock_ticker
  stop_started_harnesses
  if ! set_participant_connection reconnect app-provider,sv,app-user; then
    echo "ERROR: a participant stays disconnected. Recover with: PARTICIPANT_ACTION=reconnect PARTICIPANT_NAMES=app-provider,sv,app-user $LOCALNET_SCRIPT_DIR/canton_console.sh $LOCALNET_SCRIPT_DIR/console/participant_connection.canton" >&2
    exit_status=1
  fi
  rm -rf "$gg_work_dir"
  exit "$exit_status"
}
trap restore_connections EXIT

step() {
  echo
  echo "### Step $1: $2"
}

pass() {
  echo "PASS step $1: $2"
}

# check_sorted_parties_equal <label> <actual-json-array> <party>...: the sorted arrays are equal.
check_sorted_parties_equal() {
  local label="$1" actual="$2" expected
  shift 2
  expected="$(jq -cn '$ARGS.positional | sort' --args "$@")"
  [[ "$(jq -c 'sort' <<<"$actual")" == "$expected" ]] || fail "$label: $(jq -c 'sort' <<<"$actual"), expected $expected"
}

# created_event <transaction-json> <qualified-template-suffix>: the one created event of that template.
created_event() {
  jq -ec --arg suffix "$2" '[.events[] | .CreatedEvent? // empty | select(.templateId | endswith($suffix))]
    | if length == 1 then .[0] else error("expected one created \($suffix), found \(length)") end' <<<"$1"
}

# last_created_event <transaction-json> <qualified-template-suffix>: the last created event of that template. A
# transaction that registers and then publishes creates an Emitter and its successor.
last_created_event() {
  jq -ec --arg suffix "$2" '[.events[] | .CreatedEvent? // empty | select(.templateId | endswith($suffix))]
    | if length > 0 then .[-1] else error("expected a created \($suffix), found none") end' <<<"$1"
}

# await_core_state_on <validator>: wait until that participant shows the CoreState that sv shows.
await_core_state_on() {
  local waited_seconds expected_cid
  expected_cid="$(core_contract_id coreState)"
  for ((waited_seconds = 0; waited_seconds < OBSERVATION_WAIT_SECONDS; waited_seconds++)); do
    if [[ "$(core_contract_id coreState "$1" 2>/dev/null || true)" == "$expected_cid" ]]; then
      return 0
    fi
    sleep 1
  done
  fail "$1 does not show the CoreState ${expected_cid:0:16} after ${OBSERVATION_WAIT_SECONDS}s"
}

# gg_root <qualified-template-suffix>: the one active contract of that template that gg sees on sv, as created event.
gg_root() {
  active_contracts "$SUBMITTING_VALIDATOR" "$GG_PARTY" "$1" \
    | jq -ec --arg gg "$GG_PARTY" --arg suffix "$1" '[.[] | select(.createArgument.guardianGovernance == $gg or .createArgument.admin == $gg)]
      | if length == 1 then .[0] else error("expected one active \($suffix) of gg, found \(length)") end'
}

# await_observation <harness-name> <update-id>: the observation of that update, within OBSERVATION_WAIT_SECONDS.
await_observation() {
  local waited_seconds observation
  for ((waited_seconds = 0; waited_seconds < OBSERVATION_WAIT_SECONDS; waited_seconds++)); do
    observation="$(jq -sc --arg update "$2" '[.[] | select(.updateId == $update and (.isReobservation | not))]
      | if length == 1 then .[0] else empty end' "$WATCHER_DIR/$1.jsonl" 2>/dev/null || true)"
    if [[ -n "$observation" ]]; then
      echo "$observation"
      return 0
    fi
    sleep 1
  done
  fail "the watcher harness $1 has no observation of update $2 after ${OBSERVATION_WAIT_SECONDS}s"
}

# gg_command_id_transactions <begin-exclusive-offset>: transactions on sv after that offset that show a command id to a
# reader as gg. The participant shows the command id only to a submitting party.
gg_command_id_transactions() {
  local end_offset
  end_offset="$(curl_bounded -sSf "$(json_api_url "$SUBMITTING_VALIDATOR")/v2/state/ledger-end" -H "@$SCRIPT_AUTH_HEADER" | jq -er .offset)"
  curl_bounded --max-time 120 -sSf -X POST "$(json_api_url "$SUBMITTING_VALIDATOR")/v2/updates?stream_idle_timeout_ms=5000" \
    -H "@$SCRIPT_AUTH_HEADER" -H 'Content-Type: application/json' \
    -d "$(jq -n --arg gg "$GG_PARTY" --argjson begin "$1" --argjson end "$end_offset" '{beginExclusive: $begin, endInclusive: $end,
      updateFormat: {includeTransactions: {eventFormat: {filtersByParty: {($gg): {cumulative: [{identifierFilter:
        {WildcardFilter: {value: {includeCreatedEventBlob: false}}}}]}}, verbose: false}, transactionShape: "TRANSACTION_SHAPE_ACS_DELTA"}}}')" \
    | jq -c '[.[] | .update.Transaction?.value // empty | select((.commandId // "") != "") | {commandId, updateId, offset}]'
}

echo "Operator independence rehearsal $run_label: guardian set keys $current_set_key_indices -> $new_set_key_indices"
ceremonies_before_step_1="$(gg_ceremony_count)"
sv_offset_before_step_1="$(curl_bounded -sSf "$(json_api_url "$SUBMITTING_VALIDATOR")/v2/state/ledger-end" -H "@$SCRIPT_AUTH_HEADER" | jq -er .offset)"
echo "gg ceremonies before step 1: $ceremonies_before_step_1; sv ledger end $sv_offset_before_step_1"

step 1 "LabsOperator on app-provider; DARs vetted on app-provider"
check_party_is_local_on "$LABS_OPERATOR_PARTY" "$LABS_OPERATOR_VALIDATOR"
for validator_name in sv app-user; do
  check_party_is_not_local_on "$LABS_OPERATOR_PARTY" "$validator_name"
done
"$LOCALNET_SCRIPT_DIR/ntt_upload_dars.sh"
pass 1 "LabsOperator $LABS_OPERATOR_PARTY is local to app-provider only"

step 2 "each guardian participant vets the same DARs"
for validator_name in "${GUARDIAN_HOSTS[@]}"; do
  check_packages_vetted_on "$validator_name" "$WORMHOLE_CORE_PACKAGE_ID" "$NTT_PACKAGE_ID" "$WORMHOLE_GENESIS_PACKAGE_ID" \
    "$TOKEN_CIP0056_PACKAGE_ID"
done
[[ "$(gg_ceremony_count)" == "$ceremonies_before_step_1" ]] || fail "step 2: a gg ceremony ran during the DAR upload"
pass 2 "core, ntt, wormhole-genesis and token-cip0056 vetted on ${GUARDIAN_HOSTS[*]}; gg ceremony count unchanged"

step "3-4" "LabsOperator offers genesis; gg accepts once (gg_genesis.sh)"
"$LOCALNET_SCRIPT_DIR/gg_genesis.sh"
ceremonies_after_genesis=$(( $(gg_ceremony_count) - ceremonies_before_step_1 ))
[[ "$ceremonies_after_genesis" == 1 ]] || fail "steps 3-4: $ceremonies_after_genesis gg ceremonies, expected exactly 1"
[[ "$(active_contracts "$SUBMITTING_VALIDATOR" "$GG_PARTY" :Wormhole.Genesis:WormholeGenesisOffer | jq length)" == 0 ]] \
  || fail "steps 3-4: a WormholeGenesisOffer is still active"
for root_check in ":Wormhole.Core.State:CoreState|guardianGovernance,feeCustody" ":Wormhole.Core.State:EmitterRegistry|guardianGovernance" \
    ":Wormhole.Core.State:ReplayRootRegistry|guardianGovernance" ":Wormhole.Ntt.Governance:NttGovernance|guardianGovernance,nttCustody" \
    ":Token.CIP0056.CoinFactory:CoinFactory|guardianGovernance"; do
  IFS='|' read -r root_suffix root_signatory_names <<<"$root_check"
  root_event="$(gg_root "$root_suffix")"
  expected_signatories=()
  IFS=, read -r -a root_signatory_name_list <<<"$root_signatory_names"
  for root_signatory_name in "${root_signatory_name_list[@]}"; do
    case "$root_signatory_name" in
      guardianGovernance) expected_signatories+=("$GG_PARTY") ;;
      feeCustody) expected_signatories+=("$FEE_CUSTODY_PARTY") ;;
      nttCustody) expected_signatories+=("$CUSTODY_PARTY") ;;
    esac
  done
  check_sorted_parties_equal "$root_suffix signatories" "$(jq -c .signatories <<<"$root_event")" "${expected_signatories[@]}"
  [[ "$(jq -c '.observers // []' <<<"$root_event")" == "[]" ]] || fail "$root_suffix has observers $(jq -c .observers <<<"$root_event")"
  if jq -e --arg operator "$LABS_OPERATOR_PARTY" '[.signatories[], (.observers // [])[]] | index($operator)' <<<"$root_event" > /dev/null; then
    fail "$root_suffix names LabsOperator as a stakeholder"
  fi
  echo "  $root_suffix signatories $root_signatory_names, no observer"
done
emitter_registry_event="$(gg_root :Wormhole.Core.State:EmitterRegistry)"
[[ "$(jq -r .createArgument.onboardingFeeAdmin <<<"$emitter_registry_event")" == "$LABS_OPERATOR_PARTY" ]] \
  || fail "EmitterRegistry onboardingFeeAdmin is not LabsOperator"
pass "3-4" "one gg ceremony; offer archived; roots signed by the guardian namespace only; LabsOperator is the onboarding fee admin"

core_state_event="$(gg_root :Wormhole.Core.State:CoreState)"
current_set_index="$(jq -er .createArgument.guardianSetIndex <<<"$core_state_event")"
[[ "$(jq -er .createArgument.messageFee <<<"$core_state_event")" == "0" ]] || fail "the CoreState message fee is not 0"

echo
echo "### Preparation: local parties and wallet users (all participants connected)"
core_publisher="$(allocate_local_party sv "CorePublisher-$run_label")"
core_integrator="$(allocate_local_party sv "CoreIntegrator-$run_label")"
governance_relayer="$(allocate_local_party sv "GovernanceRelayer-$run_label")"
ntt_relayer="$(allocate_local_party sv "NttRelayer-$run_label")"
ntt_admin_user="ntt-admin-$run_label"
ntt_user_user="ntt-user-$run_label"
ntt_recipient_user="ntt-recipient-$run_label"
ntt_admin="$(wallet_onboard "$NTT_WALLET_VALIDATOR" "$ntt_admin_user")"
ntt_user="$(wallet_onboard "$NTT_WALLET_VALIDATOR" "$ntt_user_user")"
ntt_recipient="$(wallet_onboard "$NTT_WALLET_VALIDATOR" "$ntt_recipient_user")"
wallet_tap "$NTT_WALLET_VALIDATOR" "$ntt_admin_user" "$TAP_AMOUNT_USD"
wallet_tap "$NTT_WALLET_VALIDATOR" "$ntt_user_user" "$TAP_AMOUNT_USD"
wallet_tap "$NTT_WALLET_VALIDATOR" "$ntt_recipient_user" "$TAP_AMOUNT_USD"
wallet_create_transfer_preapproval "$NTT_WALLET_VALIDATOR" "$ntt_admin_user" > /dev/null
wallet_create_transfer_preapproval "$NTT_WALLET_VALIDATOR" "$ntt_recipient_user" > /dev/null
echo "sv: CorePublisher $core_publisher, CoreIntegrator $core_integrator, GovernanceRelayer $governance_relayer, NttRelayer $ntt_relayer"
echo "app-user wallet: NttAdmin $ntt_admin, NttUser $ntt_user, recipient $ntt_recipient"
build_harness

step 5 "disconnect app-provider"
set_participant_connection disconnect "$LABS_OPERATOR_VALIDATOR"
pass 5 "app-provider is disconnected"
run_harness_on sv live "$WORMHOLE_CORE_PACKAGE_ID" "$WORMHOLE_CORE_VERSION" "" 1800

step 6 "RegisterEmitter by CorePublisher"
registry_cid="$(core_contract_id emitterRegistry)"
register_transaction="$(submit_local sv "oi-register-emitter-$run_label" "$(json_array "\"$core_publisher\"")" \
  "$(exercise_command "$EMITTER_REGISTRY_TEMPLATE" "$registry_cid" RegisterEmitter "$(jq -n --arg party "$core_publisher" --arg gg "$GG_PARTY" \
    '{requester: $party, payer: $party, coreStateCid: null, feeAllocation: null, expectedGuardianGovernance: $gg}')")" \
  "$(json_array "$(gg_disclosure :Wormhole.Core.State:EmitterRegistry "$registry_cid")")")"
publisher_emitter_event="$(created_event "$register_transaction" :Wormhole.Core.State:Emitter)"
check_sorted_parties_equal "Emitter signatories" "$(jq -c .signatories <<<"$publisher_emitter_event")" "$core_publisher" "$GG_PARTY"
check_sorted_parties_equal "EmitterRegistry successor signatories" \
  "$(created_event "$register_transaction" :Wormhole.Core.State:EmitterRegistry | jq -c .signatories)" "$GG_PARTY"
pass 6 "Emitter $(jq -r .createArgument.emitterId <<<"$publisher_emitter_event") signatories [CorePublisher, gg]"

step 7 "PublishMessage at fee 0 by CorePublisher"
core_state_cid="$(core_contract_id coreState)"
publish_transaction="$(submit_local sv "oi-publish-$run_label" "$(json_array "\"$core_publisher\"")" \
  "$(exercise_command "$EMITTER_TEMPLATE" "$(jq -r .contractId <<<"$publisher_emitter_event")" PublishMessage \
    "$(jq -n --arg party "$core_publisher" --arg cs "$core_state_cid" --arg payload "$MESSAGE_PAYLOAD_HEX" \
      '{nonce: "1", payload: {tag: "MessagePayload", value: $payload}, consistencyLevel: "0", coreStateCid: $cs, payer: $party, feeAllocation: null}')")" \
  "$(core_state_disclosures "$(gg_disclosure :Wormhole.Core.State:CoreState "$core_state_cid")" "$core_state_cid")")"
publish_update_id="$(jq -er .updateId <<<"$publish_transaction")"
pass 7 "update $publish_update_id"

step 8 "the watcher on sv (read as gg) observes the message"
observation="$(await_observation live "$publish_update_id")"
jq -e --arg address "$(byte_variant_hex EmitterAddress "$(jq -c .createArgument.emitterAddress <<<"$publisher_emitter_event")")" --arg payload "$MESSAGE_PAYLOAD_HEX" \
  '.emitterChain == 75 and .emitterAddress == $address and .sequence == 0 and .payloadHex == $payload' <<<"$observation" > /dev/null \
  || fail "step 8: observation $observation differs from the Emitter address, sequence 0 or the payload"
pass 8 "chain 75, emitter $(jq -r .emitterAddress <<<"$observation") (Emitter.emitterAddress, registrar gg), sequence 0"

step 9 "ClaimReplayRoot by CoreIntegrator"
replay_namespace="$REPLAY_NAMESPACE_PREFIX-$run_label"
replay_registry_cid="$(core_contract_id replayRootRegistry)"
claim_transaction="$(submit_local sv "oi-claim-$run_label" "$(json_array "\"$core_integrator\"")" \
  "$(exercise_command "$REPLAY_ROOT_REGISTRY_TEMPLATE" "$replay_registry_cid" ClaimReplayRoot "$(jq -n --arg party "$core_integrator" \
    --arg gg "$GG_PARTY" --arg namespace "$replay_namespace" --arg threshold "$REPLAY_SPLIT_THRESHOLD" \
    '{consumer: $party, namespace: $namespace, splitThreshold: $threshold, payer: $party, coreStateCid: null, feeAllocation: null,
      expectedGuardianGovernance: $gg}')")" \
  "$(json_array "$(gg_disclosure :Wormhole.Core.State:ReplayRootRegistry "$replay_registry_cid")")")"
replay_root_event="$(created_event "$claim_transaction" :Wormhole.Core.Replay:ReplayNode)"
check_sorted_parties_equal "ReplayNode signatories" "$(jq -c .signatories <<<"$replay_root_event")" "$core_integrator" "$GG_PARTY"
pass 9 "ReplayNode signatories [CoreIntegrator, gg], namespace $replay_namespace"

step 10 "VerifyAndConsumeVAA with a devnet-signed app VAA"
app_vaa="$(generate_vaa -kind message -signing-set-index "$current_set_index" -signing-key-indices "$current_set_key_indices" -sequence "$run_label")"
# verify_and_consume_command <replay-node-cid>
verify_and_consume_command() {
  exercise_command "$CORE_STATE_TEMPLATE" "$core_state_cid" VerifyAndConsumeVAA "$(jq -n --arg vaa "$(jq -er .vaaHex <<<"$app_vaa")" \
    --arg consumer "$core_integrator" --arg namespace "$replay_namespace" --arg node "$1" \
    '{encodedVAA: {tag: "EncodedVaa", value: $vaa}, consumer: $consumer, namespace: $namespace, replayNodeCid: $node}')"
}
# verify_and_consume_disclosures <replay-node-cid>: CoreState only; CoreIntegrator signs its ReplayNode.
verify_and_consume_disclosures() {
  core_state_disclosures "$(gg_disclosure :Wormhole.Core.State:CoreState "$core_state_cid")" "$core_state_cid"
}
consume_transaction="$(submit_local sv "oi-consume-$run_label" "$(json_array "\"$core_integrator\"")" \
  "$(verify_and_consume_command "$(jq -r .contractId <<<"$replay_root_event")")" "$(verify_and_consume_disclosures)")"
consumed_node_cid="$(exercise_result "$consume_transaction" :Wormhole.Core.State:CoreState VerifyAndConsumeVAA | jq -er ._2)"
pass 10 "digest $(jq -r .digestHex <<<"$app_vaa") consumed in node ${consumed_node_cid:0:16}"

step 11 "the same VAA again"
submit_local_expect_rejection sv "oi-consume-again-$run_label" "$(json_array "\"$core_integrator\"")" \
  "$(verify_and_consume_command "$consumed_node_cid")" "$(verify_and_consume_disclosures)" "digest already consumed"
pass 11 "rejected: digest already consumed"

# relay <step> <label> <vaa-json>: SubmitGovernanceVAA by GovernanceRelayer. Prints the transaction JSON.
relay() {
  local relay_core_state_cid
  relay_core_state_cid="$(core_contract_id coreState)"
  submit_local sv "oi-relay-$2-$run_label" "$(json_array "\"$governance_relayer\"")" \
    "$(relay_governance_vaa_command "$relay_core_state_cid" "$governance_relayer" "$(jq -er .vaaHex <<<"$3")")" \
    "$(core_state_disclosures "$(gg_disclosure :Wormhole.Core.State:CoreState "$relay_core_state_cid")" "$relay_core_state_cid")"
}
# check_relay_acting_parties <step> <transaction-json>: the SubmitGovernanceVAA exercise has exactly GovernanceRelayer as actor.
check_relay_acting_parties() {
  local acting_parties
  acting_parties="$(jq -c '[.events[] | .ExercisedEvent? // empty | select(.choice == "SubmitGovernanceVAA") | .actingParties]
    | if length == 1 then .[0] else error("expected one SubmitGovernanceVAA exercise") end' <<<"$2")"
  check_sorted_parties_equal "step $1 actingParties" "$acting_parties" "$governance_relayer"
}

step 12 "relay SetMessageFee 0 (new sequence)"
set_fee_vaa="$(generate_vaa -kind set-message-fee -fee 0 -signing-set-index "$current_set_index" \
  -signing-key-indices "$current_set_key_indices" -sequence "$((run_label + 1))")"
set_fee_transaction="$(relay 12 set-fee "$set_fee_vaa")"
check_relay_acting_parties 12 "$set_fee_transaction"
[[ "$(created_event "$set_fee_transaction" :Wormhole.Core.State:CoreState | jq -r .createArgument.messageFee)" == "0" ]] \
  || fail "step 12: the new CoreState message fee is not 0"
pass 12 "actingParties [GovernanceRelayer]; messageFee 0"

step 13 "relay TransferFees"
transfer_fees_vaa="$(generate_vaa -kind transfer-fees -transfer-amount "$TRANSFER_FEES_AMOUNT" -transfer-recipient "$TRANSFER_FEES_RECIPIENT" \
  -signing-set-index "$current_set_index" -signing-key-indices "$current_set_key_indices" -sequence "$((run_label + 2))")"
transfer_fees_transaction="$(relay 13 transfer-fees "$transfer_fees_vaa")"
check_relay_acting_parties 13 "$transfer_fees_transaction"
withdrawal_event="$(created_event "$transfer_fees_transaction" :Wormhole.Core.State:FeeWithdrawalAuthorization)"
check_sorted_parties_equal "FeeWithdrawalAuthorization signatories" "$(jq -c .signatories <<<"$withdrawal_event")" "$GG_PARTY"
[[ "$(jq -c '.observers // []' <<<"$withdrawal_event")" == "[]" ]] || fail "step 13: FeeWithdrawalAuthorization has observers"
jq -e --arg amount "$TRANSFER_FEES_AMOUNT" --arg recipient "$TRANSFER_FEES_RECIPIENT" --arg digest "$(jq -r .digestHex <<<"$transfer_fees_vaa")" \
  '.createArgument.amount == $amount and .createArgument.recipient == $recipient and .createArgument.vaaHash == $digest' \
  <<<"$withdrawal_event" > /dev/null || fail "step 13: FeeWithdrawalAuthorization fields $(jq -c .createArgument <<<"$withdrawal_event")"
pass 13 "one FeeWithdrawalAuthorization, signatory gg, no observer"

step 14 "NTT RegisterManager (lock/unlock of Amulet) by NttAdmin"
dso="$(dso_party)"
amulet_instrument="$(jq -n --arg dso "$dso" '{admin: $dso, id: "Amulet"}')"
user_inputs="$(amulet_holdings "$NTT_WALLET_VALIDATOR" "$ntt_user" | jq -c '[.[].contractId]')"
lock_registry="$(amulet_transfer_factory "$ntt_user" "$ntt_admin" "$LOCK_AMOUNT" "$user_inputs")"
transfer_factory_cid="$(jq -er .factoryId <<<"$lock_registry")"
governance_cid="$(gg_root :Wormhole.Ntt.Governance:NttGovernance | jq -r .contractId)"
registry_cid="$(core_contract_id emitterRegistry)"
replay_registry_cid="$(core_contract_id replayRootRegistry)"
core_state_cid="$(core_contract_id coreState)"
ntt_admin_only="$(json_array "\"$ntt_admin\"")"
register_manager_transaction="$(submit_local "$NTT_WALLET_VALIDATOR" "oi-ntt-register-$run_label" "$ntt_admin_only" \
  "$(exercise_command "$NTT_GOVERNANCE_TEMPLATE" "$governance_cid" RegisterManager "$(jq -n \
    --arg admin "$ntt_admin" --arg emitterRegistry "$registry_cid" --arg replayRootRegistry "$replay_registry_cid" \
    --argjson instrument "$amulet_instrument" --arg factory "$transfer_factory_cid" --arg coreState "$core_state_cid" \
    --arg decimals "$AMULET_DECIMALS" '{
      admin: $admin, chainId: "75", emitterRegistryCid: $emitterRegistry, replayRootRegistryCid: $replayRootRegistry,
      instrumentId: $instrument, instrumentNonce: "0", tokenConfig: "LockUnlock", tokenDecimals: $decimals,
      factory: {tag: "TransferFactoryCid", value: $factory}, coreStateCid: $coreState, consistencyLevel: "0",
      emitterFeeAllocation: null, claimFeeAllocation: null, feeAllocation: null}')")" \
  "$(json_array \
    "$(gg_disclosure :Wormhole.Ntt.Governance:NttGovernance "$governance_cid")" \
    "$(gg_disclosure :Wormhole.Core.State:EmitterRegistry "$registry_cid")" \
    "$(gg_disclosure :Wormhole.Core.State:ReplayRootRegistry "$replay_registry_cid")" \
    "$(gg_disclosure :Wormhole.Core.State:CoreState "$core_state_cid")")")"
manager_event="$(created_event "$register_manager_transaction" :Wormhole.Ntt.Manager:NttManager)"
ledger_event="$(created_event "$register_manager_transaction" :Wormhole.Ntt.Ledger:LockedLedger)"
transceiver_emitter_event="$(last_created_event "$register_manager_transaction" :Wormhole.Core.State:Emitter)"
created_event "$register_manager_transaction" :Wormhole.Core.Replay:ReplayNode > /dev/null
check_sorted_parties_equal "transceiver Emitter signatories" "$(jq -c .signatories <<<"$transceiver_emitter_event")" "$GG_PARTY"
manager_cid="$(jq -r .contractId <<<"$manager_event")"
ledger_cid="$(jq -r .contractId <<<"$ledger_event")"
manager_address="$(byte_variant_hex NttManagerAddress "$(jq -c .createArgument.managerAddress <<<"$manager_event")")"
manager_namespace="$(jq -r .createArgument.namespace <<<"$manager_event")"
transceiver_emitter_address="$(byte_variant_hex EmitterAddress "$(jq -c .createArgument.emitterAddress <<<"$transceiver_emitter_event")")"
pass 14 "NttManager ${manager_cid:0:16}, LockedLedger, transceiver Emitter (gg), ReplayNode created"

step 15 "SetPeer by NttAdmin"
transceiver_emitter_cid="$(jq -r .contractId <<<"$transceiver_emitter_event")"
set_peer_transaction="$(submit_local "$NTT_WALLET_VALIDATOR" "oi-ntt-set-peer-$run_label" "$ntt_admin_only" \
  "$(exercise_command "$NTT_MANAGER_TEMPLATE" "$manager_cid" SetPeer "$(jq -n --arg chain "$PEER_CHAIN_ID" \
    --argjson manager "$(byte_variant_json NttManagerAddress "$PEER_MANAGER_ADDRESS")" \
    --argjson transceiver "$(byte_variant_json EmitterAddress "$PEER_TRANSCEIVER_ADDRESS")" --arg emitter "$transceiver_emitter_cid" \
    --arg coreState "$core_state_cid" --arg admin "$ntt_admin" '{
      chainId: $chain, peer: {managerAddress: $manager, transceiverAddress: $transceiver, decimals: "8"},
      transceiverEmitterCid: $emitter, coreStateCid: $coreState, consistencyLevel: "0", payer: $admin, feeAllocation: null}')")" \
  "$(json_array "$(gg_disclosure :Wormhole.Core.State:Emitter "$transceiver_emitter_cid")" \
    "$(gg_disclosure :Wormhole.Core.State:CoreState "$core_state_cid")")")"
manager_cid="$(exercise_result "$set_peer_transaction" :Wormhole.Ntt.Manager:NttManager SetPeer | jq -er ._1)"
pass 15 "peer chain $PEER_CHAIN_ID set"

step 16 "Transfer (lock $LOCK_AMOUNT CC) by NttUser"
lock_registry="$(amulet_transfer_factory "$ntt_user" "$ntt_admin" "$LOCK_AMOUNT" "$user_inputs")"
[[ "$(jq -r .factoryId <<<"$lock_registry")" == "$transfer_factory_cid" ]] || fail "step 16: the Amulet transfer factory changed"
transceiver_emitter_cid="$(created_event "$set_peer_transaction" :Wormhole.Core.State:Emitter | jq -r .contractId)"
lock_transaction="$(submit_local "$NTT_WALLET_VALIDATOR" "oi-ntt-lock-$run_label" "$(json_array "\"$ntt_user\"")" \
  "$(exercise_command "$NTT_MANAGER_TEMPLATE" "$manager_cid" Transfer "$(jq -n --arg user "$ntt_user" \
    --arg emitter "$transceiver_emitter_cid" --arg coreState "$core_state_cid" --arg ledger "$ledger_cid" --arg chain "$PEER_CHAIN_ID" \
    --argjson recipient "$(byte_variant_json UniversalAddress "$PEER_RECIPIENT_ADDRESS")" --arg rawAmount "$LOCK_RAW_AMOUNT" --arg nonce "$OUTBOUND_NONCE" \
    --argjson inputs "$user_inputs" --argjson extraArgs "$(registry_extra_args "$lock_registry")" '{
      user: $user, transceiverEmitterCid: $emitter, coreStateCid: $coreState, ledgerCid: $ledger,
      recipientChain: $chain, recipientAddress: $recipient, rawAmount: $rawAmount, nonce: $nonce, consistencyLevel: "0",
      inputHoldingCids: $inputs, feeAllocation: null, extraArgs: $extraArgs}')")" \
  "$(jq -c '. + $registry' --argjson registry "$(registry_disclosures "$lock_registry")" <<<"$(json_array \
    "$(gg_disclosure :Wormhole.Ntt.Manager:NttManager "$manager_cid")" \
    "$(gg_disclosure :Wormhole.Core.State:Emitter "$transceiver_emitter_cid")" \
    "$(gg_disclosure :Wormhole.Core.State:CoreState "$core_state_cid")" \
    "$(gg_disclosure :Wormhole.Ntt.Ledger:LockedLedger "$ledger_cid")")")")"
lock_message="$(exercise_result "$lock_transaction" :Wormhole.Ntt.Manager:NttManager Transfer)"
ledger_after_lock="$(created_event "$lock_transaction" :Wormhole.Ntt.Ledger:LockedLedger)"
ledger_cid="$(jq -r .contractId <<<"$ledger_after_lock")"
pot_cid="$(jq -er .createArgument.custodyHoldingCid <<<"$ledger_after_lock")"
[[ "$(decimal_to_units "$(jq -er .createArgument.balance <<<"$ledger_after_lock")" "$AMULET_DECIMALS")" == \
  "$(decimal_to_units "$LOCK_AMOUNT" "$AMULET_DECIMALS")" ]] || fail "step 16: LockedLedger balance is not $LOCK_AMOUNT"
lock_observation="$(await_observation live "$(jq -er .updateId <<<"$lock_transaction")")"
jq -e --arg address "$transceiver_emitter_address" --argjson message "$lock_message" \
  '.emitterChain == 75 and .emitterAddress == $address and .sequence == ($message.sequence | tonumber) and .payloadHex == $message.payload.value' \
  <<<"$lock_observation" > /dev/null || fail "step 16: observation $lock_observation differs from the WormholeMessage"
pass 16 "locked $LOCK_AMOUNT CC; the watcher observed sequence $(jq -r .sequence <<<"$lock_observation") of emitter $transceiver_emitter_address"

step 17 "Release by NttRelayer with a devnet-signed inbound transfer VAA"
release_vaa="$(generate_vaa -kind ntt-transfer -signing-set-index "$current_set_index" -signing-key-indices "$current_set_key_indices" \
  -sequence "$run_label" -emitter-chain "$PEER_CHAIN_ID" -emitter-address "$PEER_TRANSCEIVER_ADDRESS" \
  -source-manager "$PEER_MANAGER_ADDRESS" -recipient-manager "$manager_address" -source-token "$PEER_TOKEN_ADDRESS" \
  -recipient-party "$ntt_recipient" -ntt-message-id "$run_label" -trimmed-decimals 8 -trimmed-amount "$RELEASE_TRIMMED_AMOUNT")"
release_replay_node_cid="$(active_contracts "$SUBMITTING_VALIDATOR" "$GG_PARTY" :Wormhole.Core.Replay:ReplayNode \
  | jq -er --arg gg "$GG_PARTY" --arg ns "$manager_namespace" --arg digest "$(jq -r .digestHex <<<"$release_vaa")" \
    '[.[] | select(.createArgument.consumer == $gg and .createArgument.namespace == $ns)
           | select(.createArgument.prefix as $prefix | $digest | startswith($prefix))]
     | if length == 1 then .[0].contractId else error("expected one covering replay node, found \(length)") end')"
release_registry="$(amulet_transfer_factory "$ntt_admin" "$ntt_recipient" "$RELEASE_AMOUNT" "$(jq -n --arg pot "$pot_cid" '[$pot]')")"
release_transaction="$(submit_local sv "oi-ntt-release-$run_label" "$(json_array "\"$ntt_relayer\"")" \
  "$(exercise_command "$NTT_MANAGER_TEMPLATE" "$manager_cid" Release "$(jq -n --arg relayer "$ntt_relayer" \
    --arg coreState "$core_state_cid" --arg replayNode "$release_replay_node_cid" --arg vaa "$(jq -r .vaaHex <<<"$release_vaa")" \
    --arg recipient "$ntt_recipient" --arg ledger "$ledger_cid" \
    --argjson extraArgs "$(registry_extra_args "$release_registry")" '{
      executor: $relayer, coreStateCid: $coreState, replayNodeCid: $replayNode, vaaBytes: {tag: "EncodedVaa", value: $vaa},
      recipient: $recipient, ledgerCid: $ledger, inputHoldingCids: [], extraArgs: $extraArgs}')")" \
  "$(jq -c '. + $registry' --argjson registry "$(registry_disclosures "$release_registry")" <<<"$(json_array \
    "$(gg_disclosure :Wormhole.Ntt.Manager:NttManager "$manager_cid")" \
    "$(gg_disclosure :Wormhole.Core.State:CoreState "$core_state_cid")" \
    "$(gg_disclosure :Wormhole.Core.Replay:ReplayNode "$release_replay_node_cid")" \
    "$(gg_disclosure :Wormhole.Ntt.Ledger:LockedLedger "$ledger_cid")" \
    "$(disclosed_contract "$NTT_WALLET_VALIDATOR" "$ntt_admin" :Splice.Amulet:Amulet "$pot_cid")")")")"
ledger_after_release="$(created_event "$release_transaction" :Wormhole.Ntt.Ledger:LockedLedger)"
[[ "$(decimal_to_units "$(jq -er .createArgument.balance <<<"$ledger_after_release")" "$AMULET_DECIMALS")" == \
  "$(decimal_to_units "$LEDGER_BALANCE_AFTER_RELEASE" "$AMULET_DECIMALS")" ]] \
  || fail "step 17: LockedLedger balance is not $LEDGER_BALANCE_AFTER_RELEASE"
pass 17 "released $RELEASE_AMOUNT CC; LockedLedger balance $LEDGER_BALANCE_AFTER_RELEASE"
stop_harness live

step 18 "relay GuardianSetUpgrade $current_set_index to $((current_set_index + 1))"
upgrade_vaa="$(generate_vaa -kind guardian-set-upgrade -signing-set-index "$current_set_index" -signing-key-indices "$current_set_key_indices" \
  -new-set-index "$((current_set_index + 1))" -new-key-indices "$new_set_key_indices" -sequence "$((run_label + 3))")"
upgrade_transaction="$(relay 18 guardian-set-upgrade "$upgrade_vaa")"
check_relay_acting_parties 18 "$upgrade_transaction"
[[ "$(created_event "$upgrade_transaction" :Wormhole.Core.State:CoreState | jq -r .createArgument.guardianSetIndex)" == "$((current_set_index + 1))" ]] \
  || fail "step 18: guardianSetIndex is not $((current_set_index + 1))"
pass 18 "guardianSetIndex $((current_set_index + 1))"

step 19 "SetRegistrationFee by the onboarding fee admin LabsOperator"
registry_cid="$(core_contract_id emitterRegistry)"
registration_fee_before="$(gg_root :Wormhole.Core.State:EmitterRegistry | jq -r .createArgument.registrationFee)"
set_fee_response="$(submit_local_response "$LABS_OPERATOR_VALIDATOR" "oi-set-registration-fee-$run_label" \
  "$(json_array "\"$LABS_OPERATOR_PARTY\"")" \
  "$(exercise_command "$EMITTER_REGISTRY_TEMPLATE" "$registry_cid" SetRegistrationFee '{"newRegistrationFee": "1"}')" "[]" || true)"
set_fee_status="$(tail -n 1 <<<"$set_fee_response")"
[[ "$set_fee_status" != "200" ]] || fail "step 19: SetRegistrationFee committed with app-provider disconnected"
[[ "$(sed '$d' <<<"$set_fee_response" | jq -r '.code // empty' 2>/dev/null)" == "NOT_CONNECTED_TO_ANY_SYNCHRONIZER" ]] \
  || fail "step 19: expected NOT_CONNECTED_TO_ANY_SYNCHRONIZER from app-provider, got HTTP $set_fee_status $(sed '$d' <<<"$set_fee_response" | head -c 300)"
[[ "$(gg_root :Wormhole.Core.State:EmitterRegistry | jq -r .createArgument.registrationFee)" == "$registration_fee_before" ]] \
  || fail "step 19: the registration fee changed"
echo "  app-provider answered HTTP $set_fee_status: $(sed '$d' <<<"$set_fee_response" | jq -c '{code, cause: ((.cause // "")[0:160])}' 2>/dev/null || sed '$d' <<<"$set_fee_response" | head -c 300)"
pass 19 "the admin cannot submit; registration fee stays $registration_fee_before (expected stop, plan section 4.5)"

step 20 "control: reconnect app-provider, disconnect sv and app-user, publish"
set_participant_connection reconnect "$LABS_OPERATOR_VALIDATOR"
await_core_state_on "$LABS_OPERATOR_VALIDATOR"
control_publisher="$(allocate_local_party "$LABS_OPERATOR_VALIDATOR" "ControlPublisher-$run_label")"
control_registry_cid="$(core_contract_id emitterRegistry "$LABS_OPERATOR_VALIDATOR")"
control_register_transaction="$(submit_local "$LABS_OPERATOR_VALIDATOR" "oi-control-register-$run_label" "$(json_array "\"$control_publisher\"")" \
  "$(exercise_command "$EMITTER_REGISTRY_TEMPLATE" "$control_registry_cid" RegisterEmitter "$(jq -n --arg party "$control_publisher" --arg gg "$GG_PARTY" \
    '{requester: $party, payer: $party, coreStateCid: null, feeAllocation: null, expectedGuardianGovernance: $gg}')")" \
  "$(json_array "$(disclosed_contract "$LABS_OPERATOR_VALIDATOR" "$GG_PARTY" :Wormhole.Core.State:EmitterRegistry "$control_registry_cid")")")"
control_emitter_cid="$(created_event "$control_register_transaction" :Wormhole.Core.State:Emitter | jq -r .contractId)"
control_core_state_cid="$(core_contract_id coreState "$LABS_OPERATOR_VALIDATOR")"
control_core_state_disclosure="$(disclosed_contract "$LABS_OPERATOR_VALIDATOR" "$GG_PARTY" :Wormhole.Core.State:CoreState "$control_core_state_cid")"
set_participant_connection disconnect sv,app-user
start_clock_ticker "$CONTROL_TICKER_SECONDS" "$LABS_OPERATOR_VALIDATOR"
SUBMIT_AWAIT_COMPLETION=true submit_local "$LABS_OPERATOR_VALIDATOR" "oi-control-publish-$run_label" "$(json_array "\"$control_publisher\"")" \
  "$(exercise_command "$EMITTER_TEMPLATE" "$control_emitter_cid" PublishMessage "$(jq -n --arg party "$control_publisher" \
    --arg cs "$control_core_state_cid" '{nonce: "1", payload: {tag: "MessagePayload", value: "636f6e74726f6c"}, consistencyLevel: "0", coreStateCid: $cs, payer: $party, feeAllocation: null}')")" \
  "$(core_state_disclosures "$control_core_state_disclosure" "$control_core_state_cid")" > "$gg_work_dir/20.out" 2> "$gg_work_dir/20.err" || true
stop_clock_ticker
control_summary="$(completion_summary "$gg_work_dir/20.out" 2>/dev/null || echo '{}')"
jq -e '.message | test("^MEDIATOR_SAYS_TX_TIMED_OUT")' <<<"$control_summary" > /dev/null \
  || fail "step 20: expected MEDIATOR_SAYS_TX_TIMED_OUT: $control_summary $(cat "$gg_work_dir/20.err")"
jq -e --argjson expected "$(jq -cn --arg gg "$GG_PARTY" --arg fee "$FEE_CUSTODY_PARTY" '[$gg, $fee] | sort')" \
  '(.unresponsiveParties // "") | split(",") | map(select(length > 0)) | sort == $expected' <<<"$control_summary" > /dev/null \
  || fail "step 20: unresponsive parties are not exactly guardianGovernance and feeCustody: $control_summary"
set_participant_connection reconnect sv,app-user
pass 20 "$control_summary"

step 21 "gg-submitted transactions since step 1"
ceremony_count_since_step_1=$(( $(gg_ceremony_count) - ceremonies_before_step_1 ))
command_id_transactions="$(gg_command_id_transactions "$sv_offset_before_step_1")"
genesis_command_id="$(tail -n 1 "$GG_CEREMONY_LOG" | jq -er .commandId)"
echo "  (a) ceremony log: $ceremony_count_since_step_1 ($(tail -n "$ceremony_count_since_step_1" "$GG_CEREMONY_LOG" | jq -sc 'map(.commandId)'))"
echo "  (b) gg stream on sv, transactions with a command id: $(jq length <<<"$command_id_transactions") $command_id_transactions"
[[ "$ceremony_count_since_step_1" == 1 ]] || fail "step 21 (a): $ceremony_count_since_step_1 gg ceremonies, expected 1"
[[ "$(jq length <<<"$command_id_transactions")" == 1 ]] || fail "step 21 (b): expected 1 transaction with a command id"
[[ "$(jq -r '.[0].commandId' <<<"$command_id_transactions")" == "$genesis_command_id" ]] \
  || fail "step 21 (b): the transaction with a command id is not the genesis acceptance $genesis_command_id"
pass 21 "(a) == 1 and (b) == 1: the AcceptGenesis transaction $genesis_command_id"
echo
echo "gg_operator_independence.sh: PASS (21 steps)"
