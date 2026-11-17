# Shared functions for the NTT LocalNet scripts. Source this file; do not run it.
# shellcheck shell=bash
# shellcheck source=gg_lib.sh
source "$(dirname "${BASH_SOURCE[0]}")/gg_lib.sh"

NTT_CONTRACTS="$LOCALNET_STATE_DIR/ntt-contracts.json"
NTT_KEY_DIR="$LOCALNET_STATE_DIR/ntt-keys"
# Two of the three current guardianGovernance owner keys (after gg_rotate_owner_key.sh: owner1, owner2, owner6).
read -r -a NTT_GG_SIGNING_OWNERS <<<"${NTT_GG_SIGNING_OWNERS:-owner1 owner2}"
# Current guardian set index and its devnet key indices (after gg_guardian_set_rotation.sh 0 1).
NTT_GUARDIAN_SET_INDEX="${NTT_GUARDIAN_SET_INDEX:-1}"
NTT_GUARDIAN_KEY_INDICES="${NTT_GUARDIAN_KEY_INDICES:-1}"
SCAN_URL="${SCAN_URL:-http://localhost:4000}"
SCAN_HOST_HEADER="Host: scan.localhost"

readonly NTT_GOVERNANCE_TEMPLATE="#ntt:Wormhole.Ntt.Governance:NttGovernance"
readonly NTT_MANAGER_TEMPLATE="#ntt:Wormhole.Ntt.Manager:NttManager"
readonly DEPOSIT_PREAPPROVAL_TEMPLATE="#ntt:Wormhole.Ntt.Deposit:DepositPreapproval"
readonly COIN_FACTORY_TEMPLATE="#token-cip0056:Token.CIP0056.CoinFactory:CoinFactory"
readonly EMPTY_EXTRA_ARGS='{"context": {"values": {}}, "meta": {"values": {}}}'

mkdir -p "$NTT_KEY_DIR"

ntt_contract() {
  jq -er --arg name "$1" '.[$name]' "$NTT_CONTRACTS"
}

record_ntt_contract() {
  local name="$1" contract_value="$2" updated
  if [[ -f "$NTT_CONTRACTS" ]]; then
    updated="$(jq --arg name "$name" --arg value "$contract_value" '.[$name] = $value' "$NTT_CONTRACTS")"
  else
    updated="$(jq -n --arg name "$name" --arg value "$contract_value" '{($name): $value}')"
  fi
  printf '%s\n' "$updated" > "$NTT_CONTRACTS"
}

record_ntt_json() {
  local name="$1" json_value="$2" updated
  updated="$(jq --arg name "$name" --argjson value "$json_value" '.[$name] = $value' "$NTT_CONTRACTS")"
  printf '%s\n' "$updated" > "$NTT_CONTRACTS"
}

# created_contract_argument <transaction-json> <qualified-template-suffix>: create arguments of the one such created event.
created_contract_argument() {
  jq -ec --arg suffix "$2" \
    '[.events[] | .CreatedEvent? // empty | select(.templateId | endswith($suffix)) | .createArgument]
     | if length == 1 then .[0] else error("expected exactly one created \($suffix), found \(length)") end' <<<"$1"
}

gg_ceremony_signers_json() {
  gg_signers_json "${NTT_GG_SIGNING_OWNERS[@]}"
}

# external_party_signers_json <party> <key-file> <fingerprint>
external_party_signers_json() {
  jq -n --arg party "$1" --arg keyFile "$2" --arg fingerprint "$3" '{($party): [{keyFile: $keyFile, fingerprint: $fingerprint}]}'
}

create_command() {
  jq -n --arg templateId "$1" --argjson createArguments "$2" \
    '{CreateCommand: {templateId: $templateId, createArguments: $createArguments}}'
}

# active_contracts_with_blobs <validator> <party> <qualified-template-suffix>: like active_contracts, with created event blobs.
active_contracts_with_blobs() {
  local validator_name="$1" party="$2" template_suffix="$3" ledger_end admin_token
  admin_token="$(cat "$gg_work_dir/script.jwt")"
  ledger_end="$(curl -sSf "$(json_api_url "$validator_name")/v2/state/ledger-end" -H "Authorization: Bearer $admin_token" | jq -r .offset)"
  curl -sSf -X POST "$(json_api_url "$validator_name")/v2/state/active-contracts" \
    -H "Authorization: Bearer $admin_token" -H 'Content-Type: application/json' \
    -d "$(jq -n --arg party "$party" --argjson offset "$ledger_end" '{
      eventFormat: {filtersByParty: {($party): {cumulative: [{identifierFilter: {WildcardFilter: {value: {includeCreatedEventBlob: true}}}}]}}, verbose: false},
      activeAtOffset: $offset}')" \
    | jq -c --arg suffix "$template_suffix" \
      '[.[] | .contractEntry.JsActiveContract? // empty | select(.createdEvent.templateId | endswith($suffix))
        | .createdEvent + {synchronizerId}]'
}

# disclosed_contract <validator> <party> <qualified-template-suffix> <contract-id>: one DisclosedContract JSON.
disclosed_contract() {
  active_contracts_with_blobs "$1" "$2" "$3" \
    | jq -ec --arg cid "$4" '[.[] | select(.contractId == $cid)]
        | if length == 1 then .[0] | {templateId, contractId, createdEventBlob, synchronizerId}
          else error("contract \($cid) is not active for the party") end'
}

# submit_local <validator> <command-id> <act-as-json-array> <command-json> [disclosed-json]
# Submits as local parties of that participant. Prints the transaction JSON.
submit_local() {
  local validator_name="$1" command_id="$2" act_as="$3" command="$4" disclosed="${5:-[]}" response
  response="$(curl -sS -X POST "$(json_api_url "$validator_name")/v2/commands/submit-and-wait-for-transaction" \
    -H "Authorization: Bearer $(cat "$gg_work_dir/script.jwt")" -H 'Content-Type: application/json' \
    -w '\n%{http_code}' \
    -d "$(jq -n --arg commandId "$command_id" --argjson actAs "$act_as" --argjson command "$command" \
      --argjson disclosed "$disclosed" --arg userId "$SCRIPT_USER_ID" --arg synchronizerId "$LOGICAL_SYNCHRONIZER_ID" '{
      commands: {commands: [$command], commandId: $commandId, actAs: $actAs, readAs: [], userId: $userId,
                 disclosedContracts: $disclosed, synchronizerId: $synchronizerId},
      transactionFormat: {
        eventFormat: {filtersByParty: ($actAs | map({key: ., value: {cumulative: [{identifierFilter: {WildcardFilter: {value: {includeCreatedEventBlob: false}}}}]}}) | from_entries), verbose: true},
        transactionShape: "TRANSACTION_SHAPE_LEDGER_EFFECTS"}}')")"
  if [[ "$(tail -n 1 <<<"$response")" != "200" ]]; then
    echo "submit $command_id on $validator_name failed: $(sed '$d' <<<"$response")" >&2
    return 1
  fi
  sed '$d' <<<"$response" | jq -c .transaction
}

# exercise_result <transaction-json> <choice>: the exercise result of the one exercised event of that choice.
exercise_result() {
  jq -ec --arg choice "$2" '[.events[] | .ExercisedEvent? // empty | select(.choice == $choice) | .exerciseResult]
    | if length >= 1 then .[0] else error("no \($choice) exercise") end' <<<"$1"
}

# Wallet (validator) API, as the wallet user. validator_api_url <validator>
validator_api_url() {
  echo "http://localhost:$(validator_port_prefix "$1")903/api/validator/v0"
}

# wallet_onboard <validator> <user>: onboard the user and print its party id.
wallet_onboard() {
  curl -sSf -X POST "$(validator_api_url "$1")/register" -H "Authorization: Bearer $("$LOCALNET_SCRIPT_DIR/token.sh" "$2")" \
    -H 'Content-Type: application/json' -d '{}' | jq -er .party_id
}

wallet_tap() {
  curl -sSf -X POST "$(validator_api_url "$1")/wallet/tap" -H "Authorization: Bearer $("$LOCALNET_SCRIPT_DIR/token.sh" "$2")" \
    -H 'Content-Type: application/json' -d "$(jq -n --arg amount "$3" '{amount: $amount}')" >/dev/null
}

# wallet_create_transfer_preapproval <validator> <user>: print the TransferPreapproval contract id.
wallet_create_transfer_preapproval() {
  curl -sSf -X POST "$(validator_api_url "$1")/wallet/transfer-preapproval" \
    -H "Authorization: Bearer $("$LOCALNET_SCRIPT_DIR/token.sh" "$2")" -H 'Content-Type: application/json' -d '{}' \
    | jq -er .transfer_preapproval_contract_id
}

dso_party() {
  curl -sSf -H "$SCAN_HOST_HEADER" "$SCAN_URL/api/scan/v0/dso-party-id" | jq -er .dso_party_id
}

# amulet_transfer_factory <sender> <receiver> <amount> <input-holding-cids-json>: the registry's factory,
# transfer kind and choice context for an Amulet transfer.
amulet_transfer_factory() {
  local dso now later
  dso="$(dso_party)"
  now="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  later="$(date -u -v+1H +%Y-%m-%dT%H:%M:%SZ 2>/dev/null || date -u -d '+1 hour' +%Y-%m-%dT%H:%M:%SZ)"
  curl -sSf -X POST -H "$SCAN_HOST_HEADER" -H 'Content-Type: application/json' \
    "$SCAN_URL/registry/transfer-instruction/v1/transfer-factory" \
    -d "$(jq -n --arg dso "$dso" --arg sender "$1" --arg receiver "$2" --arg amount "$3" --argjson inputs "$4" \
      --arg now "$now" --arg later "$later" '{
      choiceArguments: {expectedAdmin: $dso,
        transfer: {sender: $sender, receiver: $receiver, amount: $amount, instrumentId: {admin: $dso, id: "Amulet"},
                   requestedAt: $now, executeBefore: $later, inputHoldingCids: $inputs, meta: {values: {}}},
        extraArgs: {context: {values: {}}, meta: {values: {}}}},
      excludeDebugFields: true}')"
}

# Registry disclosures in DisclosedContract form.
registry_disclosures() {
  jq -c '[.choiceContext.disclosedContracts[] | {templateId, contractId, createdEventBlob, synchronizerId}]' <<<"$1"
}

registry_extra_args() {
  jq -c '{context: .choiceContext.choiceContextData, meta: {values: {}}}' <<<"$1"
}

# amulet_holdings <validator> <party>: [{contractId, amount}] of the party's unlocked Amulets.
amulet_holdings() {
  active_contracts "$1" "$2" ":Splice.Amulet:Amulet" \
    | jq -c --arg owner "$2" '[.[] | select(.createArgument.owner == $owner)
        | {contractId, amount: .createArgument.amount.initialAmount}]'
}

# covering_replay_node <namespace> <digest-hex>: the active ReplayNode of guardianGovernance that covers the digest.
covering_replay_node() {
  active_contracts "$SUBMITTING_VALIDATOR" "$OPERATOR_PARTY" ":Wormhole.Core.Replay:ReplayNode" \
    | jq -er --arg gg "$GG_PARTY" --arg ns "$1" --arg digest "$2" \
      '[.[] | select(.createArgument.consumer == $gg and .createArgument.namespace == $ns)
             | select(.createArgument.prefix as $prefix | $digest | startswith($prefix))]
       | if length == 1 then .[0].contractId else error("expected one covering replay node, found \(length)") end'
}

# ntt_transfer_vaa <ntt-transfer flags...>: a VAA from the current guardian set.
ntt_transfer_vaa() {
  generate_vaa -kind ntt-transfer -signing-set-index "$NTT_GUARDIAN_SET_INDEX" \
    -signing-key-indices "$NTT_GUARDIAN_KEY_INDICES" "$@"
}

# message_summary <wormhole-message-json>: the WormholeMessage fields the watcher reads.
message_summary() {
  jq -c '{registrar: .registrar[0:24], owner: .owner[0:24], emitterId, sequence, nonce, consistencyLevel,
          payloadPrefix: .payload[0:8], payloadBytes: ((.payload | length) / 2)}' <<<"$1"
}

# allocate_local_party <validator> <party-hint>: allocate a party on that participant and print its id.
allocate_local_party() {
  curl -sSf -X POST "$(json_api_url "$1")/v2/parties" -H "Authorization: Bearer $("$LOCALNET_SCRIPT_DIR/token.sh")" \
    -H 'Content-Type: application/json' -d "$(jq -n --arg hint "$2" '{partyIdHint: $hint, identityProviderId: ""}')" \
    | jq -er .partyDetails.party
}

# allocate_external_party <validator> <party-hint> <key-file-prefix>: print {partyId, fingerprint, keyFile}.
allocate_external_party() {
  node "$LOCALNET_SCRIPT_DIR/allocate_external_party.js" "$(json_api_url "$1")" "$gg_work_dir/script.jwt" \
    "$LOGICAL_SYNCHRONIZER_ID" "$2" "$3"
}

# current_emitter <emitter-id>: the active Emitter of guardianGovernance with that id, as created event JSON.
current_emitter() {
  active_contracts "$SUBMITTING_VALIDATOR" "$OPERATOR_PARTY" ":Wormhole.Core.State:Emitter" \
    | jq -ec --arg gg "$GG_PARTY" --arg id "$1" '[.[] | select(.createArgument.owner == $gg and .createArgument.emitterId == $id)]
        | if length == 1 then .[0] else error("expected one active emitter \($id), found \(length)") end'
}

current_ntt_governance() {
  active_contracts "$SUBMITTING_VALIDATOR" "$OPERATOR_PARTY" ":Wormhole.Ntt.Governance:NttGovernance" \
    | jq -er --arg gg "$GG_PARTY" '[.[] | select(.createArgument.guardianGovernance == $gg)]
        | if length == 1 then .[0].contractId else error("expected one NttGovernance, found \(length)") end'
}

current_emitter_registry() {
  active_contracts "$SUBMITTING_VALIDATOR" "$OPERATOR_PARTY" ":Wormhole.Core.State:EmitterRegistry" \
    | jq -er 'if length == 1 then .[0].contractId else error("expected one EmitterRegistry, found \(length)") end'
}

# operator_disclosure <qualified-template-suffix> <contract-id>: disclosure of a contract that Operator sees.
operator_disclosure() {
  disclosed_contract "$SUBMITTING_VALIDATOR" "$OPERATOR_PARTY" "$1" "$2"
}

# json_array <json>...: one JSON array of the arguments.
json_array() {
  jq -sc '.' <<<"$(printf '%s\n' "$@")"
}
