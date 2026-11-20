#!/usr/bin/env bash
# CIP-0056 registry API (v1) of the disclosure service on LocalNet. A party on app-user that is not a stakeholder of the
# canonical CoinFactory sends the bridged burn/mint token to a second party with only the registry API responses: once
# as an offer that the receiver accepts, once direct through a TransferPreapproval. totalSupply is checked before and
# after the mint, and after the transfers.
#
# Usage: registry_api_check.sh
# Precondition: ntt_burn_mint.sh ran, and gateway_t1.sh up ran: the service reads the
# participant through the T1 mutual-TLS ledger proxy, because it sends a token only over https or to loopback.
# DISCLOSURE_SERVICE_IMAGE names an image of canton/disclosure/service with the registry API.
# Set NTT_GUARDIAN_SET_INDEX and NTT_GUARDIAN_KEY_INDICES to the current guardian set of CoreState (ntt_lib.sh).
# The script starts one container on the localnet network and removes it at exit.
set -euo pipefail
# shellcheck source=ntt_lib.sh
source "$(dirname "${BASH_SOURCE[0]}")/ntt_lib.sh"

readonly SERVICE_CONTAINER="registry-api-check-service"
readonly SERVICE_HOST_PORT="${REGISTRY_API_CHECK_PORT:-7601}"
readonly SERVICE_CONTAINER_PORT=7599
readonly SERVICE_URL="http://127.0.0.1:$SERVICE_HOST_PORT"
readonly PARTICIPANT_NETWORK="localnet"
readonly LEDGER_PROXY_CONTAINER="t1-ledger-proxy"
readonly PARTICIPANT_JSON_API="https://$LEDGER_PROXY_CONTAINER:7443"
readonly GATEWAY_CERT_DIR="$LOCALNET_STATE_DIR/gateway-t1/certs"
readonly GATEWAY_LEDGER_USER="disclosure-gateway"
readonly HEALTHZ_MAX_POLLS=30
# The service keeps ledger reads for --cache-ttl. The script sets 1 s and waits longer before a read that must see a write.
readonly SERVICE_CACHE_TTL="1s"
readonly CACHE_SETTLE_SECONDS=2
readonly DAML_DECIMAL_FRACTION_DIGITS=10
readonly BURN_MINT_PEER_CHAIN_ID=2
readonly BURN_MINT_PEER_MANAGER_ADDRESS="000000000000000000000000e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0"
readonly BURN_MINT_PEER_TRANSCEIVER_ADDRESS="000000000000000000000000d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1"
readonly BURN_MINT_PEER_TOKEN_ADDRESS="000000000000000000000000c2c2c2c2c2c2c2c2c2c2c2c2c2c2c2c2c2c2c2c2"
readonly MINT_TRIMMED_AMOUNT=250000000
readonly MINT_AMOUNT="2.5"
readonly OFFER_AMOUNT="1.0"
readonly DIRECT_AMOUNT="0.5"
readonly TRANSFER_WINDOW_SECONDS=3600
readonly TRANSFER_FACTORY_INTERFACE="#splice-api-token-transfer-instruction-v1:Splice.Api.Token.TransferInstructionV1:TransferFactory"
readonly TRANSFER_INSTRUCTION_INTERFACE="#splice-api-token-transfer-instruction-v1:Splice.Api.Token.TransferInstructionV1:TransferInstruction"
readonly TRANSFER_PREAPPROVAL_TEMPLATE="#token-cip0056:Token.CIP0056.CoinTransfer:TransferPreapproval"
readonly PREAPPROVAL_CONTEXT_KEY="token-cip0056.transferPreapprovalCid"

ntt_requires_custody_party "$NTT_VERSION" || fail "ntt $NTT_VERSION has no nttCustody; the registry API reads as guardianGovernance and nttCustody"
[[ -n "${DISCLOSURE_SERVICE_IMAGE:-}" ]] || fail "set DISCLOSURE_SERVICE_IMAGE to the local disclosure service image"
docker image inspect "$DISCLOSURE_SERVICE_IMAGE" > /dev/null || fail "image $DISCLOSURE_SERVICE_IMAGE does not exist"
! docker container inspect "$SERVICE_CONTAINER" > /dev/null 2>&1 || fail "container $SERVICE_CONTAINER exists; remove it first"
[[ "$(docker container inspect -f '{{.State.Running}}' "$LEDGER_PROXY_CONTAINER" 2> /dev/null)" == "true" ]] \
  || fail "container $LEDGER_PROXY_CONTAINER is not running; run gateway_t1.sh up"

run_label="$(date +%s)"
check_passes=0
pass() { echo "PASS: $*"; check_passes=$((check_passes + 1)); }

remove_service_container() {
  if docker container inspect "$SERVICE_CONTAINER" > /dev/null 2>&1; then
    docker rm -f "$SERVICE_CONTAINER" > /dev/null
    echo "removed container $SERVICE_CONTAINER"
  fi
  rm -rf "$gg_work_dir"
}
trap remove_service_container EXIT

# decimal_text_to_units <decimal-text>: the amount in 10^-10 units.
decimal_text_to_units() { decimal_to_units "$1" "$DAML_DECIMAL_FRACTION_DIGITS"; }

# registry_request <method> <path> [json-body]: prints the body, then the HTTP status on the last line.
registry_request() {
  if [[ $# -eq 3 ]]; then
    curl_bounded -sS -w '\n%{http_code}' -X "$1" "$SERVICE_URL$2" -H 'Content-Type: application/json' -d "$3"
  else
    curl_bounded -sS -w '\n%{http_code}' -X "$1" "$SERVICE_URL$2"
  fi
}

# registry_ok <method> <path> [json-body]: the response body. Fails unless HTTP 200.
registry_ok() {
  local response
  response="$(registry_request "$@")"
  [[ "$(tail -n 1 <<<"$response")" == "200" ]] || fail "$1 $2: HTTP $(tail -n 1 <<<"$response") $(sed '$d' <<<"$response")"
  sed '$d' <<<"$response" | jq -c .
}

instrument_total_supply_units() {
  decimal_text_to_units "$(registry_ok GET "/registry/metadata/v1/instruments/$(jq -rn --arg id "$instrument_id" '$id | @uri')" | jq -er .totalSupply)"
}

party_coin_units() {
  local amounts
  amounts="$(active_contracts app-user "$1" ":Token.CIP0056.Coin:Coin" \
    | jq -c --arg gg "$GG_PARTY" --arg id "$instrument_id" --arg owner "$1" \
      '[.[] | .createArgument | select(.instrumentId.admin == $gg and .instrumentId.id == $id and .owner == $owner) | .amount]')"
  if [[ "$amounts" == "[]" ]]; then echo 0; else json_decimal_array_total_units "$DAML_DECIMAL_FRACTION_DIGITS" "$amounts"; fi
}

party_coin_cids() {
  active_contracts app-user "$1" ":Token.CIP0056.Coin:Coin" \
    | jq -c --arg gg "$GG_PARTY" --arg id "$instrument_id" --arg owner "$1" \
      '[.[] | select(.createArgument.instrumentId.admin == $gg and .createArgument.instrumentId.id == $id and .createArgument.owner == $owner) | .contractId]'
}

# wallet_disclosures <choice-context-json>: the DisclosedContract fields that the Ledger API takes.
wallet_disclosures() {
  jq -c '[.disclosedContracts[] | {templateId, contractId, createdEventBlob, synchronizerId}]' <<<"$1"
}

# transfer_choice_arguments <sender> <receiver> <amount> <input-cids-json> <context-json>
transfer_choice_arguments() {
  jq -cn --arg gg "$GG_PARTY" --arg id "$instrument_id" --arg sender "$1" --arg receiver "$2" --arg amount "$3" \
    --argjson inputs "$4" --argjson context "$5" --argjson window "$TRANSFER_WINDOW_SECONDS" '{
      expectedAdmin: $gg,
      transfer: {sender: $sender, receiver: $receiver, amount: $amount, instrumentId: {admin: $gg, id: $id},
        requestedAt: (now - 60 | todate), executeBefore: (now + $window | todate), inputHoldingCids: $inputs,
        meta: {values: {}}},
      extraArgs: {context: $context, meta: {values: {}}}}'
}

# registry_transfer <sender> <receiver> <amount> <expected-kind> <command-id>: one TransferFactory_Transfer that
# uses only the transfer-factory response. Prints the transaction JSON.
registry_transfer() {
  local sender="$1" receiver="$2" amount="$3" expected_kind="$4" command_id="$5" inputs request factory_response
  inputs="$(party_coin_cids "$sender")"
  request="$(jq -cn --argjson arguments "$(transfer_choice_arguments "$sender" "$receiver" "$amount" "$inputs" '{"values": {}}')" \
    '{choiceArguments: $arguments, excludeDebugFields: true}')"
  factory_response="$(registry_ok POST /registry/transfer-instruction/v1/transfer-factory "$request")"
  echo "transfer-factory: kind $(jq -r .transferKind <<<"$factory_response") factory $(jq -r .factoryId <<<"$factory_response")" \
    "context $(jq -c .choiceContext.choiceContextData <<<"$factory_response")" \
    "disclosures $(jq -c '[.choiceContext.disclosedContracts[].templateId]' <<<"$factory_response")" >&2
  [[ "$(jq -r .transferKind <<<"$factory_response")" == "$expected_kind" ]] \
    || fail "transferKind $(jq -r .transferKind <<<"$factory_response"), expected $expected_kind"
  if [[ "$expected_kind" == direct ]]; then
    jq -e --arg key "$PREAPPROVAL_CONTEXT_KEY" '.choiceContext as $context | $context.choiceContextData.values[$key] as $value
        | $value.tag == "AV_ContractId" and ([$context.disclosedContracts[].contractId] | index($value.value) != null)'       <<<"$factory_response" > /dev/null || fail "direct context lacks $PREAPPROVAL_CONTEXT_KEY with a disclosed AV_ContractId"
  fi
  submit_local app-user "$command_id" "$(jq -cn --arg sender "$sender" '[$sender]')" \
    "$(exercise_command "$TRANSFER_FACTORY_INTERFACE" "$(jq -r .factoryId <<<"$factory_response")" TransferFactory_Transfer \
      "$(transfer_choice_arguments "$sender" "$receiver" "$amount" "$inputs" "$(jq -c .choiceContext.choiceContextData <<<"$factory_response")")")" \
    "$(wallet_disclosures "$(jq -c .choiceContext <<<"$factory_response")")"
}

echo "== 1. Ledger user, burn/mint instrument and instruments file"
json_api="$(json_api_url app-provider)"
gateway_rights="$(curl_bounded -sSf "$json_api/v2/users/$GATEWAY_LEDGER_USER/rights" -H "@$ADMIN_AUTH_HEADER" \
  | jq -c '[.rights[].kind | to_entries[0] | {(.key): (.value.value.party // "")}] | sort_by(.CanReadAs)')" \
  || fail "ledger user $GATEWAY_LEDGER_USER does not exist; run gateway_t1.sh ledger-user"
expected_rights="$(jq -cn --arg gg "$GG_PARTY" --arg custody "$NTT_CUSTODY_PARTY" '[{CanReadAs: $gg}, {CanReadAs: $custody}] | sort_by(.CanReadAs)')"
[[ "$gateway_rights" == "$expected_rights" ]] || fail "$GATEWAY_LEDGER_USER rights $gateway_rights, expected $expected_rights"
pass "ledger user $GATEWAY_LEDGER_USER has only $gateway_rights"
service_dir="$gg_work_dir/service"
mkdir -p "$service_dir"
(umask 077 && "$LOCALNET_SCRIPT_DIR/token.sh" "$GATEWAY_LEDGER_USER" > "$service_dir/gateway.jwt")
restrict_secret_to_service_user "$service_dir/gateway.jwt"
for gateway_cert_file in ca.crt gateway-client.crt gateway-client.key; do
  [[ -f "$GATEWAY_CERT_DIR/$gateway_cert_file" ]] || fail "$GATEWAY_CERT_DIR/$gateway_cert_file is absent; run gateway_t1.sh up"
done

manager_address="$(ntt_contract burnMint | jq -er .managerAddress)"
burn_mint_managers="$(active_contracts app-provider "$GG_PARTY" ":Wormhole.Ntt.Manager:NttManager" \
  | jq -c --arg gg "$GG_PARTY" '[.[].createArgument | select(.tokenConfig == "BurnMint" and .instrumentId.admin == $gg)]')"
instrument_id="$(jq -er --arg address "$manager_address" '[.[] | select(.managerAddress == $address) | .instrumentId.id]
  | if length == 1 then .[0] else error("expected one burn/mint manager \($address)") end' <<<"$burn_mint_managers")"
echo "burn/mint deployment $manager_address instrument $instrument_id; gg burn/mint managers $(jq length <<<"$burn_mint_managers")"
jq -n --argjson managers "$burn_mint_managers" --arg id "$instrument_id" \
  '[$managers[] | .instrumentId.id] | unique | map({key: ., value: (if . == $id then {name: "Wormhole Bridged Test", symbol: "WBT"}
    else {name: ("Bridged " + .[0:12]), symbol: "BRG"} end)}) | from_entries' > "$service_dir/instruments.json"
chmod 644 "$service_dir/instruments.json"
chmod 755 "$service_dir"

echo "== 2. Start the service with the registry API on $SERVICE_URL"
# --behind-edge-proxy: the container binds 0.0.0.0, and Docker publishes the port on 127.0.0.1 only.
docker run -d --name "$SERVICE_CONTAINER" --network "$PARTICIPANT_NETWORK" \
  -p "127.0.0.1:$SERVICE_HOST_PORT:$SERVICE_CONTAINER_PORT" \
  -v "$service_dir/gateway.jwt:/etc/disclosure/gateway.jwt:ro" \
  -v "$service_dir/instruments.json:/etc/disclosure/instruments.json:ro" \
  -v "$GATEWAY_CERT_DIR/ca.crt:/etc/disclosure/ca.crt:ro" \
  -v "$GATEWAY_CERT_DIR/gateway-client.crt:/etc/disclosure/gateway-client.crt:ro" \
  -v "$GATEWAY_CERT_DIR/gateway-client.key:/etc/disclosure/gateway-client.key:ro" \
  "$DISCLOSURE_SERVICE_IMAGE" \
  --json-api "$PARTICIPANT_JSON_API" --access-token-file /etc/disclosure/gateway.jwt \
  --json-api-ca-file /etc/disclosure/ca.crt --json-api-client-cert-file /etc/disclosure/gateway-client.crt \
  --json-api-client-key-file /etc/disclosure/gateway-client.key \
  --guardian-governance-party "$GG_PARTY" --ntt-custody-party "$NTT_CUSTODY_PARTY" \
  --registry-instruments-file /etc/disclosure/instruments.json \
  --cache-ttl "$SERVICE_CACHE_TTL" --listen "0.0.0.0:$SERVICE_CONTAINER_PORT" --behind-edge-proxy > /dev/null
for (( poll = 1; poll <= HEALTHZ_MAX_POLLS; poll++ )); do
  curl_bounded -sSf "$SERVICE_URL/v1/healthz" > /dev/null 2>&1 && break
  (( poll < HEALTHZ_MAX_POLLS )) || { docker logs --tail 30 "$SERVICE_CONTAINER" >&2; fail "service not healthy"; }
  sleep 1
done
docker logs "$SERVICE_CONTAINER" 2>&1 | sed 's/^/service: /'

echo "== 3. Metadata API"
registry_info="$(registry_ok GET /registry/metadata/v1/info)"
echo "info $registry_info"
jq -e --arg gg "$GG_PARTY" '.adminId == $gg and (.supportedApis | keys | length) == 4' <<<"$registry_info" > /dev/null || fail "info $registry_info"
pass "info adminId is guardianGovernance, 4 supported APIs"
instruments="$(registry_ok GET "/registry/metadata/v1/instruments?pageSize=100")"
echo "instruments $(jq -c '[.instruments[] | {id, symbol, decimals, totalSupply}]' <<<"$instruments")"
jq -e --arg id "$instrument_id" '[.instruments[] | select(.id == $id and .decimals == 8 and .symbol == "WBT")] | length == 1' \
  <<<"$instruments" > /dev/null || fail "instrument $instrument_id is not listed with decimals 8"
pass "instrument $instrument_id listed with decimals 8 from NttManager.tokenDecimals"
response="$(registry_request GET /registry/metadata/v1/instruments/wormhole-no-such-instrument)"
[[ "$(tail -n 1 <<<"$response")" == "404" ]] || fail "unknown instrument: HTTP $(tail -n 1 <<<"$response")"
pass "unknown instrument: HTTP 404 $(sed '$d' <<<"$response")"
supply_before_mint="$(instrument_total_supply_units)"
echo "totalSupply before the mint: $supply_before_mint units"

echo "== 4. Mint $MINT_AMOUNT to party A on app-user (disclosure service mint flow)"
party_a="$(allocate_local_party app-user "RegistryA-$run_label")"
party_b="$(allocate_local_party app-user "RegistryB-$run_label")"
relayer_party="$(allocate_local_party app-user "RegistryRelayer-$run_label")"
echo "A $party_a"
echo "B $party_b"
instrument_json="$(jq -cn --arg gg "$GG_PARTY" --arg id "$instrument_id" '{admin: $gg, id: $id}')"
submit_local app-user "registry-deposit-preapproval-$run_label" "$(jq -cn --arg party_a "$party_a" '[$party_a]')" \
  "$(create_command "$DEPOSIT_PREAPPROVAL_TEMPLATE" "$(jq -cn --arg party_a "$party_a" --argjson instrument "$instrument_json" \
    '{owner: $party_a, instrumentId: $instrument}')")" > /dev/null
sleep "$CACHE_SETTLE_SECONDS"
vaa="$(ntt_transfer_vaa -sequence "$run_label" -emitter-chain "$BURN_MINT_PEER_CHAIN_ID" \
  -emitter-address "$BURN_MINT_PEER_TRANSCEIVER_ADDRESS" -source-manager "$BURN_MINT_PEER_MANAGER_ADDRESS" \
  -recipient-manager "$manager_address" -source-token "$BURN_MINT_PEER_TOKEN_ADDRESS" -recipient-party "$party_a" \
  -ntt-message-id "$run_label" -trimmed-decimals 8 -trimmed-amount "$MINT_TRIMMED_AMOUNT")"
mint_flow="$(registry_ok GET "/v1/flows/mint?manager=$manager_address&digest=$(jq -r .digestHex <<<"$vaa")&recipient=$(jq -rn --arg party_a "$party_a" '$party_a | @uri')")"
flow_cid() { jq -er --arg role "$1" '[.disclosures[] | select(.role == $role) | .contractId] | if length == 1 then .[0] else error("role \($role)") end' <<<"$mint_flow"; }
submit_local app-user "registry-mint-$run_label" "$(jq -cn --arg relayer "$relayer_party" '[$relayer]')" \
  "$(exercise_command "$NTT_MANAGER_TEMPLATE" "$(flow_cid NttManager)" Mint "$(jq -n --arg relayer "$relayer_party" \
    --arg coreState "$(flow_cid CoreState)" --arg replayNode "$(flow_cid "covering ReplayNode")" \
    --arg vaa "$(jq -r .vaaHex <<<"$vaa")" --arg user "$party_a" \
    --arg preapproval "$(flow_cid DepositPreapproval)" --argjson extraArgs "$EMPTY_EXTRA_ARGS" '{
      executor: $relayer, coreStateCid: $coreState, replayNodeCid: $replayNode, vaaBytes: $vaa,
      recipient: $user, depositPreapprovalCid: $preapproval, extraArgs: $extraArgs}')")" \
  "$(jq -c '[.disclosures[] | {templateId, contractId, createdEventBlob, synchronizerId}]' <<<"$mint_flow")" > /dev/null
[[ "$(party_coin_units "$party_a")" == "$(decimal_text_to_units "$MINT_AMOUNT")" ]] || fail "A holds $(party_coin_units "$party_a") units after the mint"
pass "A holds $MINT_AMOUNT after Mint"
sleep "$CACHE_SETTLE_SECONDS"
supply_after_mint="$(instrument_total_supply_units)"
(( supply_after_mint - supply_before_mint == $(decimal_text_to_units "$MINT_AMOUNT") )) \
  || fail "totalSupply $supply_before_mint -> $supply_after_mint units, expected +$(decimal_text_to_units "$MINT_AMOUNT")"
pass "totalSupply $supply_before_mint -> $supply_after_mint units (+$MINT_AMOUNT)"

echo "== 5. A is not a stakeholder of the CoinFactory"
factory_count="$(active_contracts app-user "$party_a" ":Token.CIP0056.CoinFactory:CoinFactory" | jq length)"
[[ "$factory_count" == "0" ]] || fail "A sees $factory_count CoinFactory contracts"
pass "A sees 0 CoinFactory contracts"

echo "== 6. Offer: A sends $OFFER_AMOUNT to B; B accepts with the accept choice context"
offer_transaction="$(registry_transfer "$party_a" "$party_b" "$OFFER_AMOUNT" offer "registry-offer-$run_label")"
instruction_cid="$(created_contract_id "$offer_transaction" ":Token.CIP0056.CoinTransfer:CoinTransferInstruction")"
pass "TransferFactory_Transfer (offer) update $(jq -r .updateId <<<"$offer_transaction"), CoinTransferInstruction $instruction_cid"
sleep "$CACHE_SETTLE_SECONDS"
accept_context="$(registry_ok POST "/registry/transfer-instruction/v1/$instruction_cid/choice-contexts/accept" '{"excludeDebugFields": true}')"
echo "accept context $accept_context"
response="$(registry_request POST "/registry/transfer-instruction/v1/$(flow_cid NttManager)/choice-contexts/accept" '{}')"
[[ "$(tail -n 1 <<<"$response")" == "404" ]] || fail "accept context for an NttManager id: HTTP $(tail -n 1 <<<"$response")"
pass "accept context for a contract that is not a CoinTransferInstruction: HTTP 404"
accept_transaction="$(submit_local app-user "registry-accept-$run_label" "$(jq -cn --arg party_b "$party_b" '[$party_b]')" \
  "$(exercise_command "$TRANSFER_INSTRUCTION_INTERFACE" "$instruction_cid" TransferInstruction_Accept \
    "$(jq -cn --argjson context "$(jq -c .choiceContextData <<<"$accept_context")" '{extraArgs: {context: $context, meta: {values: {}}}}')")" \
  "$(wallet_disclosures "$accept_context")")"
[[ "$(party_coin_units "$party_b")" == "$(decimal_text_to_units "$OFFER_AMOUNT")" ]] || fail "B holds $(party_coin_units "$party_b") units after accept"
pass "TransferInstruction_Accept update $(jq -r .updateId <<<"$accept_transaction"): B holds $OFFER_AMOUNT"

echo "== 7. Direct: B creates a TransferPreapproval; A sends $DIRECT_AMOUNT"
preapproval_transaction="$(submit_local app-user "registry-transfer-preapproval-$run_label" "$(jq -cn --arg party_b "$party_b" '[$party_b]')" \
  "$(create_command "$TRANSFER_PREAPPROVAL_TEMPLATE" "$(jq -cn --arg party_b "$party_b" --argjson instrument "$instrument_json" \
    '{owner: $party_b, instrumentId: $instrument}')")")"
preapproval_cid="$(created_contract_id "$preapproval_transaction" ":Token.CIP0056.CoinTransfer:TransferPreapproval")"
echo "B TransferPreapproval $preapproval_cid"
sleep "$CACHE_SETTLE_SECONDS"
direct_transaction="$(registry_transfer "$party_a" "$party_b" "$DIRECT_AMOUNT" direct "registry-direct-$run_label")"
jq -e '[.events[] | .ExercisedEvent? // empty | select(.choice == "TransferPreapproval_Receive")] | length == 1' \
  <<<"$direct_transaction" > /dev/null || fail "the direct transfer did not exercise TransferPreapproval_Receive"
[[ "$(party_coin_units "$party_b")" == "$(( $(decimal_text_to_units "$OFFER_AMOUNT") + $(decimal_text_to_units "$DIRECT_AMOUNT") ))" ]] \
  || fail "B holds $(party_coin_units "$party_b") units after the direct transfer"
[[ "$(created_contract_id "$direct_transaction" ":Token.CIP0056.CoinTransfer:CoinTransferInstruction" 2> /dev/null || true)" == "" ]] \
  || fail "the direct transfer created a CoinTransferInstruction"
pass "direct update $(jq -r .updateId <<<"$direct_transaction") exercised TransferPreapproval_Receive $preapproval_cid; B holds $OFFER_AMOUNT + $DIRECT_AMOUNT"
expected_a_units="$(( $(decimal_text_to_units "$MINT_AMOUNT") - $(decimal_text_to_units "$OFFER_AMOUNT") - $(decimal_text_to_units "$DIRECT_AMOUNT") ))"
[[ "$(party_coin_units "$party_a")" == "$expected_a_units" ]] || fail "A holds $(party_coin_units "$party_a") units, expected $expected_a_units"
pass "A holds $expected_a_units units of change"

echo "== 8. totalSupply after the transfers"
sleep "$CACHE_SETTLE_SECONDS"
supply_after_transfers="$(instrument_total_supply_units)"
(( supply_after_transfers == supply_after_mint )) \
  || fail "totalSupply $supply_after_mint -> $supply_after_transfers units across transfers"
pass "totalSupply unchanged across the transfers: $supply_after_transfers units"

echo "registry_api_check.sh: PASS ($check_passes checks)"
