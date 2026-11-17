# Shared functions for the guardianGovernance rehearsal scripts. Source this file; do not run it.
# shellcheck shell=bash
# shellcheck source=env.sh
source "$(dirname "${BASH_SOURCE[0]}")/env.sh"

# Every run appends its stdout and stderr to an evidence log. The logs hold no tokens or private keys.
readonly EVIDENCE_DIR="$LOCALNET_STATE_DIR/evidence"
mkdir -p "$EVIDENCE_DIR"
EVIDENCE_LOG="$EVIDENCE_DIR/$(basename "$0" .sh)-$(date -u +%Y%m%dT%H%M%SZ).log"
exec > >(tee -a "$EVIDENCE_LOG") 2> >(tee -a "$EVIDENCE_LOG" >&2)
echo "Run $(basename "$0") $* at $(date -u +%Y-%m-%dT%H:%M:%SZ), commit $(git -C "$REPO_ROOT" rev-parse --short HEAD 2>/dev/null || echo unknown)"
echo "Evidence log: $EVIDENCE_LOG"

GG_ARTIFACTS="$LOCALNET_STATE_DIR/gg-artifacts.json"
OPERATOR_ARTIFACTS="$LOCALNET_STATE_DIR/operator-artifacts.json"
CORE_CONTRACTS="$LOCALNET_STATE_DIR/core-contracts.json"
# Interactive submissions go through the participant that hosts the Operator party.
SUBMITTING_VALIDATOR="app-provider"

for required_file in "$GG_ARTIFACTS" "$OPERATOR_ARTIFACTS"; do
  if [[ ! -f "$required_file" ]]; then
    echo "Missing $required_file. Run gg_host.sh first." >&2
    exit 1
  fi
done

GG_PARTY="$(jq -r .partyId "$GG_ARTIFACTS")"
OPERATOR_PARTY="$(jq -r .partyId "$OPERATOR_ARTIFACTS")"
SYNCHRONIZER_ID="$(jq -r .synchronizerId "$GG_ARTIFACTS")"
# The Ledger API takes the logical synchronizer id, without the "::<protocol>-<serial>" suffix.
LOGICAL_SYNCHRONIZER_ID="$(sed -E 's/::[0-9]+-[0-9]+$//' <<<"$SYNCHRONIZER_ID")"

gg_work_dir="$(mktemp -d)"
trap 'rm -rf "$gg_work_dir"' EXIT
(umask 077 && "$LOCALNET_SCRIPT_DIR/token.sh" "$SCRIPT_USER_ID" > "$gg_work_dir/script.jwt")
# curl -H @file headers: Daml Script user and the ledger-api-user administrator.
SCRIPT_AUTH_HEADER="$gg_work_dir/script.auth"
ADMIN_AUTH_HEADER="$gg_work_dir/admin.auth"
write_auth_header_file "$SCRIPT_AUTH_HEADER" "$SCRIPT_USER_ID"
write_auth_header_file "$ADMIN_AUTH_HEADER"

# Signer JSON for one owner key: signer_json <public-key-file>.
signer_json() {
  local public_key_file="$1" fingerprint
  fingerprint="$(jq -r --arg file "$public_key_file" \
    '[.owners[] | select(.publicKeyFile == $file) | .namespace][0] // empty' "$GG_ARTIFACTS" "$OPERATOR_ARTIFACTS" | head -n 1)"
  if [[ -z "$fingerprint" ]]; then
    fingerprint="$(jq -r --arg file "$public_key_file" '.[$file] // empty' "$LOCALNET_STATE_DIR/extra-key-fingerprints.json" 2>/dev/null || true)"
  fi
  if [[ -z "$fingerprint" ]]; then
    echo "No fingerprint known for $public_key_file" >&2
    return 1
  fi
  jq -n --arg keyFile "${public_key_file%.pub}.key" --arg fingerprint "$fingerprint" \
    '{keyFile: $keyFile, fingerprint: $fingerprint}'
}

operator_signers_json() {
  jq -n --arg party "$OPERATOR_PARTY" --argjson signer "$(signer_json "$LOCALNET_STATE_DIR/operator-keys/owner1.pub")" \
    '{($party): [$signer]}'
}

# gg_signers_json <owner-name>... : signatures of guardianGovernance owner keys under gg-keys/.
gg_signers_json() {
  local owner_name signers="[]"
  for owner_name in "$@"; do
    signers="$(jq --argjson signer "$(signer_json "$LOCALNET_STATE_DIR/gg-keys/$owner_name.pub")" '. + [$signer]' <<<"$signers")"
  done
  jq -n --arg party "$GG_PARTY" --argjson signers "$signers" '{($party): $signers}'
}

# submit_interactive <command-id> <act-as-json-array> <command-json> <signers-json> [disclosed-json] [package-preference-json]
# Prints the transaction JSON. Returns non-zero on rejection; the error text goes to stderr.
# With SUBMIT_AWAIT_COMPLETION=true, prints the command completion (status included) instead.
submit_interactive() {
  local command_id="$1" act_as="$2" command="$3"
  shift 3
  submit_interactive_commands "$command_id" "$act_as" "$(jq -c -n --argjson command "$command" '[$command]')" "$@"
}

# submit_interactive_commands <command-id> <act-as-json-array> <commands-json-array> <signers-json> [disclosed-json] [package-preference-json]
# submit_interactive with 1 or 2 commands in one atomic transaction.
submit_interactive_commands() {
  local command_id="$1" act_as="$2" commands="$3" signers="$4"
  local disclosed="${5:-[]}" package_preference="${6:-[]}" await_completion="${SUBMIT_AWAIT_COMPLETION:-false}"
  local spec_file="$gg_work_dir/$command_id.spec.json"
  jq -n --arg commandId "$command_id" --argjson actAs "$act_as" --argjson commands "$commands" \
    --argjson signers "$signers" --argjson disclosed "$disclosed" --argjson preference "$package_preference" \
    --arg synchronizerId "$LOGICAL_SYNCHRONIZER_ID" --argjson awaitCompletion "$await_completion" \
    '{commandId: $commandId, actAs: $actAs, commands: $commands, signers: $signers,
      disclosedContracts: $disclosed, packageIdSelectionPreference: $preference, synchronizerId: $synchronizerId,
      awaitCompletion: $awaitCompletion}' \
    > "$spec_file"
  node "$LOCALNET_SCRIPT_DIR/interactive_submit.js" \
    "$(json_api_url "$SUBMITTING_VALIDATOR")" "$gg_work_dir/script.jwt" "$spec_file"
}

# created_contract_id <transaction-json> <qualified-template-suffix>, e.g. ":Wormhole.Core.State:CoreState".
created_contract_id() {
  jq -er --arg suffix "$2" \
    '[.events[] | .CreatedEvent? // empty | select(.templateId | endswith($suffix)) | .contractId] | if length == 1 then .[0] else error("expected exactly one created \($suffix), found \(length)") end' \
    <<<"$1"
}

exercise_command() {
  local template_id="$1" contract_id="$2" choice="$3" choice_argument="$4"
  jq -n --arg templateId "$template_id" --arg contractId "$contract_id" --arg choice "$choice" \
    --argjson choiceArgument "$choice_argument" \
    '{ExerciseCommand: {templateId: $templateId, contractId: $contractId, choice: $choice, choiceArgument: $choiceArgument}}'
}

# core_contract_id <name>: CoreState and the Operator-owned Emitter resolve from the app-provider ACS
# (exactly one each); a submission that commits late can replace them. Other names come from core-contracts.json.
core_contract_id() {
  local template_suffix="" required_owner=""
  case "$1" in
    coreState) template_suffix=":Wormhole.Core.State:CoreState" ;;
    # Other deployments (NTT) register guardianGovernance-owned Emitters on the same registry.
    emitter) template_suffix=":Wormhole.Core.State:Emitter"; required_owner="$OPERATOR_PARTY" ;;
  esac
  if [[ -n "$template_suffix" ]]; then
    active_contracts "$SUBMITTING_VALIDATOR" "$OPERATOR_PARTY" "$template_suffix" \
      | jq -er --arg name "$1" --arg owner "$required_owner" \
        '[.[] | select($owner == "" or .createArgument.owner == $owner)]
         | if length == 1 then .[0].contractId else error("expected one active \($name), found \(length)") end'
  else
    jq -er --arg name "$1" '.[$name]' "$CORE_CONTRACTS"
  fi
}

record_core_contract_id() {
  local name="$1" contract_id="$2" updated
  if [[ -f "$CORE_CONTRACTS" ]]; then
    updated="$(jq --arg name "$name" --arg cid "$contract_id" '.[$name] = $cid' "$CORE_CONTRACTS")"
  else
    updated="$(jq -n --arg name "$name" --arg cid "$contract_id" '{($name): $cid}')"
  fi
  printf '%s\n' "$updated" > "$CORE_CONTRACTS"
}

readonly CORE_STATE_TEMPLATE="#wormhole-core:Wormhole.Core.State:CoreState"
# shellcheck disable=SC2034 # used by the scripts that source this file
readonly EMITTER_REGISTRY_TEMPLATE="#wormhole-core:Wormhole.Core.State:EmitterRegistry"
# shellcheck disable=SC2034
readonly REPLAY_ROOT_REGISTRY_TEMPLATE="#wormhole-core:Wormhole.Core.State:ReplayRootRegistry"
readonly EMITTER_TEMPLATE="#wormhole-core:Wormhole.Core.State:Emitter"

# active_contracts <validator> <party> <qualified-template-suffix>: active contracts of that template that the party sees there.
active_contracts() {
  local validator_name="$1" party="$2" template_suffix="$3" ledger_end
  ledger_end="$(curl -sSf "$(json_api_url "$validator_name")/v2/state/ledger-end" -H "@$SCRIPT_AUTH_HEADER" | jq -r .offset)"
  curl -sSf -X POST "$(json_api_url "$validator_name")/v2/state/active-contracts" \
    -H "@$SCRIPT_AUTH_HEADER" -H 'Content-Type: application/json' \
    -d "$(jq -n --arg party "$party" --argjson offset "$ledger_end" '{
      eventFormat: {filtersByParty: {($party): {cumulative: [{identifierFilter: {WildcardFilter: {value: {includeCreatedEventBlob: false}}}}]}}, verbose: false},
      activeAtOffset: $offset}')" \
    | jq -c --arg suffix "$template_suffix" \
      '[.[] | .contractEntry.JsActiveContract.createdEvent? // empty | select(.templateId | endswith($suffix))]'
}

# set_participant_connection <disconnect|reconnect> <comma-separated validator names>
# Returns non-zero unless each named participant reports the expected connection state afterwards.
set_participant_connection() {
  local action="$1" participant_names="$2" expected_connected console_output console_status=0 participant_name
  local -a participant_list
  case "$action" in
    disconnect) expected_connected=false ;;
    reconnect) expected_connected=true ;;
    *) echo "set_participant_connection: unknown action $action" >&2; return 2 ;;
  esac
  IFS=, read -ra participant_list <<<"$participant_names"
  if [[ ${#participant_list[@]} -eq 0 ]]; then
    echo "set_participant_connection: no participant names" >&2
    return 2
  fi
  console_output="$(PARTICIPANT_ACTION="$action" PARTICIPANT_NAMES="$participant_names" \
    "$LOCALNET_SCRIPT_DIR/canton_console.sh" "$LOCALNET_SCRIPT_DIR/console/participant_connection.canton" 2>&1)" \
    || console_status=$?
  grep -E '^CONNECTION|ERROR' <<<"$console_output" || true
  if [[ "$console_status" -ne 0 ]]; then
    echo "FAIL: participant_connection.canton exited with status $console_status" >&2
    return 1
  fi
  for participant_name in "${participant_list[@]}"; do
    if ! grep -qE "^CONNECTION $participant_name connected=$expected_connected\$" <<<"$console_output"; then
      echo "FAIL: $participant_name does not report connected=$expected_connected after $action" >&2
      return 1
    fi
  done
}

# verify_vaa_as_operator <command-id> <vaa-hex> <pub-keys-json>: ParseAndVerifyVAA on the current CoreState.
# guardianGovernance is a CoreState signatory, so its hosts confirm this transaction.
verify_vaa_as_operator() {
  submit_interactive "$1" "$(jq -n --arg op "$OPERATOR_PARTY" '[$op]')" \
    "$(exercise_command "$CORE_STATE_TEMPLATE" "$(core_contract_id coreState)" ParseAndVerifyVAA \
      "$(jq -n --arg op "$OPERATOR_PARTY" --arg vaa "$2" --argjson keys "$3" '{verifier: $op, encodedVAA: $vaa, pubKeys: $keys}')")" \
    "$(operator_signers_json)"
}

# generate_vaa <canton_localnet_vaa flags...>: prints the JSON {vaaHex, digestHex, pubKeys} of a devnet-key VAA.
generate_vaa() {
  (cd "$REPO_ROOT/node" && GOFLAGS=-mod=readonly GOPROXY=off go run ./hack/canton_localnet_vaa "$@")
}

# Daml [(Int, Bytes)] public-key hints from generate_vaa output.
vaa_pub_keys_json() {
  jq -c '[.pubKeys[] | {_1: (.guardianIndex | tostring), _2: .publicKeyHex}]' <<<"$1"
}

# submit_governance_vaa <command-id> <vaa-hex> <pub-keys-json>: SubmitGovernanceVAA on the current CoreState.
submit_governance_vaa() {
  submit_interactive "$1" "$(jq -n --arg op "$OPERATOR_PARTY" '[$op]')" \
    "$(exercise_command "$CORE_STATE_TEMPLATE" "$(core_contract_id coreState)" SubmitGovernanceVAA \
      "$(jq -n --arg vaa "$2" --argjson keys "$3" '{encodedVAA: $vaa, pubKeys: $keys}')")" \
    "$(operator_signers_json)"
}

# publish_message <command-id> <payload-hex>: PublishMessage on the recorded Emitter at fee 0.
publish_message() {
  submit_interactive "$1" "$(jq -n --arg op "$OPERATOR_PARTY" '[$op]')" \
    "$(exercise_command "$EMITTER_TEMPLATE" "$(core_contract_id emitter)" PublishMessage \
      "$(jq -n --arg op "$OPERATOR_PARTY" --arg cs "$(core_contract_id coreState)" --arg payload "$2" \
        '{nonce: "2", payload: $payload, consistencyLevel: "0", coreStateCid: $cs, payer: $op, feeAllocation: null}')")" \
    "$(operator_signers_json)"
}

# completion_summary <completion-output-file>: elapsed time, status code and message of an awaited completion.
completion_summary() {
  jq -c '{elapsedMs, code: (.completion.status.code // 0), message: (.completion.status.message // ""),
          unresponsiveParties: ([.completion.status.details[]?.valueDecoded.metadata.unresponsiveParties? // empty] | first)}' "$1"
}

# start_clock_ticker <seconds>: run the synchronizer clock ticker in the background; sets clock_ticker_pid.
start_clock_ticker() {
  CLOCK_TICKER_SECONDS="$1" "$LOCALNET_SCRIPT_DIR/canton_console.sh" \
    "$LOCALNET_SCRIPT_DIR/console/synchronizer_clock_ticker.canton" > "$gg_work_dir/clock-ticker.log" 2>&1 &
  clock_ticker_pid=$!
}

stop_clock_ticker() {
  if [[ -n "${clock_ticker_pid:-}" ]]; then
    pkill -P "$clock_ticker_pid" 2>/dev/null || true
    kill "$clock_ticker_pid" 2>/dev/null || true
    wait "$clock_ticker_pid" 2>/dev/null || true
    clock_ticker_pid=""
  fi
}
