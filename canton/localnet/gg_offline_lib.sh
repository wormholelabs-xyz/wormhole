# Argument builders of the gg_*.sh scripts that read only their arguments. gg_lib.sh and test/gg_offline_lib_test.sh
# source this file; do not run it. Needs bash and jq.
# shellcheck shell=bash

readonly CORE_STATE_TEMPLATE="#wormhole-core:Wormhole.Core.State:CoreState"
# shellcheck disable=SC2034 # used by the scripts that source this file
readonly EMITTER_REGISTRY_TEMPLATE="#wormhole-core:Wormhole.Core.State:EmitterRegistry"
# shellcheck disable=SC2034
readonly REPLAY_ROOT_REGISTRY_TEMPLATE="#wormhole-core:Wormhole.Core.State:ReplayRootRegistry"
# shellcheck disable=SC2034
readonly EMITTER_TEMPLATE="#wormhole-core:Wormhole.Core.State:Emitter"
# shellcheck disable=SC2034
readonly GENESIS_OFFER_TEMPLATE="#wormhole-genesis:Wormhole.Genesis:WormholeGenesisOffer"
# Governance emitter of the devnet guardian set (sdk/vaa GovernanceChain and GovernanceEmitter).
readonly GOVERNANCE_CHAIN_ID="1"
readonly GOVERNANCE_CONTRACT="0000000000000000000000000000000000000000000000000000000000000004"
readonly MAX_GUARDIAN_SET_INDEX=4294967295

# check_party_id <label> <party-id>: <hint>::1220<64 lowercase hex>.
check_party_id() {
  if [[ ! "$2" =~ ^[A-Za-z][A-Za-z0-9_-]*::1220[0-9a-f]{64}$ ]]; then
    echo "$1 is not <hint>::1220<64 hex>: '$2'" >&2
    return 1
  fi
}

# check_parties_differ <label> <party-id> <label> <party-id>
check_parties_differ() {
  if [[ "$2" == "$4" ]]; then
    echo "$1 is $3: $2" >&2
    return 1
  fi
}

# create_command <template-id> <create-arguments-json>
create_command() {
  jq -cn --arg templateId "$1" --argjson createArguments "$2" \
    '{CreateCommand: {templateId: $templateId, createArguments: $createArguments}}'
}

exercise_command() {
  jq -cn --arg templateId "$1" --arg contractId "$2" --arg choice "$3" --argjson choiceArgument "$4" \
    '{ExerciseCommand: {templateId: $templateId, contractId: $contractId, choice: $choice, choiceArgument: $choiceArgument}}'
}

# first_genesis_parameters <guardian-governance> <fee-custody> <ntt-custody> <onboarding-fee-admin>
# <fee-instrument-admin> <fee-instrument-id> <guardian-set-index> <guardians: address:public-key, comma-separated>
# Genesis requires each guardian public key. The core checks that each key derives its address.
# The GenesisParameters JSON (wormhole-genesis 0.2.x) of a first genesis: zero fees, id floors 0, empty governance
# replay records without an eviction floor.
first_genesis_parameters() {
  if [[ $# -ne 8 ]]; then
    echo "usage: first_genesis_parameters <gg> <fee-custody> <ntt-custody> <onboarding-fee-admin> <fee-instrument-admin> <fee-instrument-id> <guardian-set-index> <guardians>" >&2
    return 2
  fi
  local gg="$1" fee_custody="$2" ntt_custody="$3" fee_admin="$4" instrument_admin="$5" instrument_id="$6"
  local guardian_set_index="$7" guardians_text="$8" guardian guardian_address guardian_public_key
  local -a guardians guardian_addresses guardian_public_keys
  check_party_id guardianGovernance "$gg" || return 1
  check_party_id feeCustody "$fee_custody" || return 1
  check_party_id nttCustody "$ntt_custody" || return 1
  check_party_id onboardingFeeAdmin "$fee_admin" || return 1
  check_party_id "fee instrument admin" "$instrument_admin" || return 1
  check_parties_differ feeCustody "$fee_custody" guardianGovernance "$gg" || return 1
  check_parties_differ nttCustody "$ntt_custody" guardianGovernance "$gg" || return 1
  check_parties_differ feeCustody "$fee_custody" nttCustody "$ntt_custody" || return 1
  check_parties_differ "fee instrument admin" "$instrument_admin" guardianGovernance "$gg" || return 1
  check_parties_differ "fee instrument admin" "$instrument_admin" feeCustody "$fee_custody" || return 1
  if [[ ! "$instrument_id" =~ ^[A-Za-z0-9_-]+$ ]]; then
    echo "fee instrument id has characters outside [A-Za-z0-9_-]: '$instrument_id'" >&2
    return 1
  fi
  if [[ ! "$guardian_set_index" =~ ^(0|[1-9][0-9]{0,9})$ ]] || (( guardian_set_index > MAX_GUARDIAN_SET_INDEX )); then
    echo "guardian set index is not 0 to $MAX_GUARDIAN_SET_INDEX: '$guardian_set_index'" >&2
    return 1
  fi
  IFS=, read -r -a guardians <<<"$guardians_text"
  if [[ ${#guardians[@]} -eq 0 ]]; then
    echo "no guardian" >&2
    return 1
  fi
  for guardian in "${guardians[@]}"; do
    if [[ "$guardian" != *:* ]]; then
      echo "guardian $guardian has no public key" >&2
      return 1
    fi
    guardian_addresses+=("${guardian%%:*}")
    guardian_public_keys+=("${guardian#*:}")
  done
  for guardian_public_key in "${guardian_public_keys[@]}"; do
    if [[ ! "$guardian_public_key" =~ ^04[0-9a-f]{128}$ ]]; then
      echo "guardian public key is not 04 and 64 bytes of lowercase hex: '$guardian_public_key'" >&2
      return 1
    fi
  done
  if [[ "$(printf '%s\n' "${guardian_public_keys[@]}" | sort -u | wc -l | tr -d ' ')" != "${#guardian_public_keys[@]}" ]]; then
    echo "duplicate guardian public key" >&2
    return 1
  fi
  for guardian_address in "${guardian_addresses[@]}"; do
    if [[ ! "$guardian_address" =~ ^[0-9a-f]{40}$ ]]; then
      echo "guardian address is not 40 lowercase hex: '$guardian_address'" >&2
      return 1
    fi
    if [[ "$guardian_address" == "0000000000000000000000000000000000000000" ]]; then
      echo "guardian address is the zero address" >&2
      return 1
    fi
  done
  if [[ "$(printf '%s\n' "${guardian_addresses[@]}" | sort -u | wc -l | tr -d ' ')" != "${#guardian_addresses[@]}" ]]; then
    echo "duplicate guardian address" >&2
    return 1
  fi
  jq -cn --arg gg "$gg" --arg feeCustody "$fee_custody" --arg nttCustody "$ntt_custody" --arg feeAdmin "$fee_admin" \
    --arg instrumentAdmin "$instrument_admin" --arg instrumentId "$instrument_id" --arg index "$guardian_set_index" \
    --arg chain "$GOVERNANCE_CHAIN_ID" --arg contract "$GOVERNANCE_CONTRACT" \
    --argjson guardians "$(paste -d: <(printf '%s\n' "${guardian_addresses[@]}") <(printf '%s\n' "${guardian_public_keys[@]}") \
      | jq -Rsc 'split("\n") | map(select(length > 0) | split(":")
          | {address: {tag: "GuardianAddress", value: .[0]}, publicKey: {tag: "GuardianPublicKey", value: .[1]}})')" '{
      guardianGovernance: $gg, feeCustody: $feeCustody, nttCustody: $nttCustody, onboardingFeeAdmin: $feeAdmin,
      genesisGuardianSet: {guardianSetIndex: $index, guardians: $guardians},
      governanceChainId: $chain, governanceContract: {tag: "EmitterAddress", value: $contract}, initialMessageFee: "0",
      feeInstrument: {admin: $instrumentAdmin, id: $instrumentId}, registrationFee: "0", replayRootClaimFee: "0",
      firstEmitterId: "0", firstNttManagerId: "0",
      coreGovernanceReplayRecord: {consumedGovernance: [], evictedGovernanceMaxTimestampSeconds: null},
      nttGovernanceReplayRecord: {consumedGovernance: [], evictedGovernanceMaxTimestampSeconds: null},
      genesisKind: {tag: "FirstGenesis", value: {}}}'
}

# genesis_offer_arguments <operator> <genesis-parameters-json>: create arguments of the WormholeGenesisOffer.
genesis_offer_arguments() {
  local operator="$1" parameters="$2" party_name
  check_party_id operator "$operator" || return 1
  for party_name in guardianGovernance feeCustody nttCustody; do
    if ! jq -e --arg name "$party_name" 'type == "object" and (.[$name] | type == "string")' <<<"$parameters" > /dev/null 2>&1; then
      echo "genesis parameters have no $party_name" >&2
      return 1
    fi
    check_parties_differ operator "$operator" "$party_name" "$(jq -r --arg name "$party_name" '.[$name]' <<<"$parameters")" || return 1
  done
  jq -cn --arg operator "$operator" --argjson parameters "$parameters" '{operator: $operator, parameters: $parameters}'
}

# accept_genesis_arguments <offer-create-argument-json> <agreed-parameters-json>: the AcceptGenesis argument.
# The agreed parameters are the guardians' own copy. The offer must carry exactly the operator and those parameters.
accept_genesis_arguments() {
  local offer="$1" agreed_parameters="$2"
  if ! jq -e 'type == "object" and (keys == ["operator", "parameters"])' <<<"$offer" > /dev/null 2>&1; then
    echo "genesis offer keys are not exactly operator and parameters: $(jq -c 'keys? // .' <<<"$offer" 2>/dev/null || echo "$offer")" >&2
    return 1
  fi
  if ! jq -e 'type == "object" and length > 0' <<<"$agreed_parameters" > /dev/null 2>&1; then
    echo "agreed genesis parameters are not a JSON object" >&2
    return 1
  fi
  if [[ "$(jq -cS .parameters <<<"$offer")" != "$(jq -cS . <<<"$agreed_parameters")" ]]; then
    echo "genesis offer parameters differ from the agreed parameters" >&2
    diff <(jq -S .parameters <<<"$offer") <(jq -S . <<<"$agreed_parameters") >&2 || true
    return 1
  fi
  jq -cn --argjson agreed "$agreed_parameters" '{agreedParameters: $agreed}'
}

# relay_governance_vaa_command <core-state-cid> <submitter> <vaa-hex>: the SubmitGovernanceVAA command.
relay_governance_vaa_command() {
  local core_state_cid="$1" submitter="$2" vaa_hex="$3"
  if [[ ! "$core_state_cid" =~ ^00[0-9a-f]+$ ]]; then
    echo "CoreState contract id is not 00<hex>: '$core_state_cid'" >&2
    return 1
  fi
  check_party_id submitter "$submitter" || return 1
  if [[ -z "$vaa_hex" ]]; then
    echo "governance VAA is empty" >&2
    return 1
  fi
  if [[ ! "$vaa_hex" =~ ^([0-9a-f]{2})+$ ]]; then
    echo "governance VAA is not lowercase hex of whole bytes" >&2
    return 1
  fi
  exercise_command "$CORE_STATE_TEMPLATE" "$core_state_cid" SubmitGovernanceVAA \
    "$(jq -cn --arg submitter "$submitter" --arg vaa "$vaa_hex" \
      '{submitter: $submitter, encodedVAA: {tag: "EncodedVaa", value: $vaa}}')"
}

# attach_guardian_public_keys_command <core-state-cid> <submitter> <guardian-set-index> <public-keys-hex-json-array>:
# the AttachGuardianPublicKeys command. The core checks that each key derives an address of the set.
attach_guardian_public_keys_command() {
  local core_state_cid="$1" submitter="$2" guardian_set_index="$3" public_keys="$4"
  if [[ ! "$core_state_cid" =~ ^00[0-9a-f]+$ ]]; then
    echo "CoreState contract id is not 00<hex>: '$core_state_cid'" >&2
    return 1
  fi
  check_party_id submitter "$submitter" || return 1
  if [[ ! "$guardian_set_index" =~ ^(0|[1-9][0-9]{0,9})$ ]] || (( guardian_set_index > MAX_GUARDIAN_SET_INDEX )); then
    echo "guardian set index is not 0 to $MAX_GUARDIAN_SET_INDEX: '$guardian_set_index'" >&2
    return 1
  fi
  if ! jq -e 'type == "array" and length > 0 and length <= 255
      and all(.[]; type == "string" and test("^04[0-9a-f]{128}$"))' <<<"$public_keys" > /dev/null 2>&1; then
    echo "public keys are not an array of 1 to 255 keys of 04 and 64 bytes of lowercase hex: $public_keys" >&2
    return 1
  fi
  if ! jq -e 'length == (unique | length)' <<<"$public_keys" > /dev/null; then
    echo "duplicate public key: $public_keys" >&2
    return 1
  fi
  exercise_command "$CORE_STATE_TEMPLATE" "$core_state_cid" AttachGuardianPublicKeys \
    "$(jq -c --arg submitter "$submitter" --arg index "$guardian_set_index" \
      '{submitter: $submitter, targetGuardianSetIndex: $index, guardianPublicKeys: [.[] | {tag: "GuardianPublicKey", value: .}]}' \
      <<<"$public_keys")"
}

# core_state_disclosures <disclosure-json> <core-state-cid>: a one-element disclosed contracts array for that CoreState.
core_state_disclosures() {
  local disclosure="$1" core_state_cid="$2"
  if ! jq -e 'type == "object" and keys == ["contractId", "createdEventBlob", "synchronizerId", "templateId"]' \
      <<<"$disclosure" > /dev/null 2>&1; then
    echo "CoreState disclosure is not {templateId, contractId, createdEventBlob, synchronizerId}: '$disclosure'" >&2
    return 1
  fi
  if [[ "$(jq -r .contractId <<<"$disclosure")" != "$core_state_cid" ]]; then
    echo "CoreState disclosure is for $(jq -r .contractId <<<"$disclosure"), expected $core_state_cid" >&2
    return 1
  fi
  if ! jq -e '.templateId | endswith(":Wormhole.Core.State:CoreState")' <<<"$disclosure" > /dev/null; then
    echo "disclosure template is $(jq -r .templateId <<<"$disclosure"), expected Wormhole.Core.State:CoreState" >&2
    return 1
  fi
  if ! jq -e '(.createdEventBlob | type == "string" and length > 0) and (.synchronizerId | type == "string" and length > 0)' \
      <<<"$disclosure" > /dev/null; then
    echo "CoreState disclosure has an empty created event blob or synchronizer id" >&2
    return 1
  fi
  jq -c '[.]' <<<"$disclosure"
}
