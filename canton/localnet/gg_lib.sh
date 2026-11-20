# Shared functions for the guardianGovernance rehearsal scripts. Source this file; do not run it.
# shellcheck shell=bash
# shellcheck source=env.sh
source "$(dirname "${BASH_SOURCE[0]}")/env.sh"
# shellcheck source=owner_key_input_checks.sh
source "$(dirname "${BASH_SOURCE[0]}")/owner_key_input_checks.sh"
# shellcheck source=gg_offline_lib.sh
source "$(dirname "${BASH_SOURCE[0]}")/gg_offline_lib.sh"

# Every run appends its stdout and stderr to an evidence log. Tokens go only to mode 0600 files in $gg_work_dir,
# private keys only to mode 0600 key files.
readonly EVIDENCE_DIR="$LOCALNET_STATE_DIR/evidence"
mkdir -p "$EVIDENCE_DIR"
EVIDENCE_LOG="$EVIDENCE_DIR/$(basename "$0" .sh)-$(date -u +%Y%m%dT%H%M%SZ).log"
exec > >(tee -a "$EVIDENCE_LOG") 2> >(tee -a "$EVIDENCE_LOG" >&2)
echo "Run $(basename "$0") $* at $(date -u +%Y-%m-%dT%H:%M:%SZ), commit $(git -C "$REPO_ROOT" describe --always --dirty 2>/dev/null || echo unknown)"
echo "Evidence log: $EVIDENCE_LOG"

GG_ARTIFACTS="$LOCALNET_STATE_DIR/gg-artifacts.json"
# Local parties of gg_host.sh: LabsOperator on app-provider; CorePublisher, CoreVerifier and GovernanceRelayer on sv.
LOCAL_PARTIES="$LOCALNET_STATE_DIR/local-parties.json"
CORE_CONTRACTS="$LOCALNET_STATE_DIR/core-contracts.json"
# One JSON line for each committed submission that guardian namespace parties sign (submit_guardian_ceremony).
GG_CEREMONY_LOG="$LOCALNET_STATE_DIR/gg-ceremonies.jsonl"
# Guardian ceremonies and the guardianGovernance reads go through this participant. It hosts guardianGovernance.
SUBMITTING_VALIDATOR="${SUBMITTING_VALIDATOR:-sv}"
# Host of CorePublisher, CoreVerifier and GovernanceRelayer.
readonly CORE_ROLE_VALIDATOR="sv"
# Each LocalNet participant hosts guardianGovernance. Thus each participant is a guardian host and an informee host.
# shellcheck disable=SC2034 # used by the scripts that source this file
readonly GUARDIAN_HOSTS=(app-provider sv app-user)

fail() {
  echo "FAIL: $*" >&2
  exit 1
}

readonly CURL_CONNECT_SECONDS=10
readonly CURL_MAX_SECONDS=30
# curl_bounded <curl arguments...>: curl with a connect timeout and a maximum time. A later --max-time argument
# replaces CURL_MAX_SECONDS.
curl_bounded() {
  curl --connect-timeout "$CURL_CONNECT_SECONDS" --max-time "$CURL_MAX_SECONDS" "$@"
}

for required_file in "$GG_ARTIFACTS" "$LOCAL_PARTIES"; do
  if [[ ! -f "$required_file" ]]; then
    echo "Missing $required_file. Run gg_host.sh first." >&2
    exit 1
  fi
done

GG_PARTY="$(jq -er .partyId "$GG_ARTIFACTS")"
# shellcheck disable=SC2034 # used by the scripts that source this file
LABS_OPERATOR_PARTY="$(jq -er .labsOperator "$LOCAL_PARTIES")"
CORE_PUBLISHER_PARTY="$(jq -er .corePublisher "$LOCAL_PARTIES")"
CORE_VERIFIER_PARTY="$(jq -er .coreVerifier "$LOCAL_PARTIES")"
GOVERNANCE_RELAYER_PARTY="$(jq -er .governanceRelayer "$LOCAL_PARTIES")"
readonly LABS_OPERATOR_PARTY CORE_PUBLISHER_PARTY CORE_VERIFIER_PARTY GOVERNANCE_RELAYER_PARTY
SYNCHRONIZER_ID="$(jq -er .synchronizerId "$GG_ARTIFACTS")"
# Physical synchronizer id: <name>::<namespace>::<protocol>-<serial>.
if [[ ! "$SYNCHRONIZER_ID" =~ ^[^:]+::[0-9a-f]+::[0-9]+-[0-9]+$ ]]; then
  echo "synchronizerId in $GG_ARTIFACTS is not <name>::<namespace>::<protocol>-<serial>: $SYNCHRONIZER_ID" >&2
  exit 1
fi
# The Ledger API takes the logical synchronizer id, without the "::<protocol>-<serial>" suffix.
LOGICAL_SYNCHRONIZER_ID="$(sed -E 's/::[0-9]+-[0-9]+$//' <<<"$SYNCHRONIZER_ID")"
if [[ "$LOGICAL_SYNCHRONIZER_ID" == "$SYNCHRONIZER_ID" ]]; then
  echo "cannot strip the protocol suffix from $SYNCHRONIZER_ID" >&2
  exit 1
fi

gg_work_dir="$(mktemp -d)"
trap 'rm -rf "$gg_work_dir"' EXIT
(umask 077 && "$LOCALNET_SCRIPT_DIR/token.sh" "$SCRIPT_USER_ID" > "$gg_work_dir/script.jwt")
# curl -H @file headers: Daml Script user and the ledger-api-user administrator.
SCRIPT_AUTH_HEADER="$gg_work_dir/script.auth"
ADMIN_AUTH_HEADER="$gg_work_dir/admin.auth"
write_auth_header_file "$SCRIPT_AUTH_HEADER" "$SCRIPT_USER_ID"
write_auth_header_file "$ADMIN_AUTH_HEADER"

# Signer JSON for one owner key: build_signer_json <public-key-file>.
build_signer_json() {
  local public_key_file="$1" fingerprint
  fingerprint="$(jq -r --arg file "$public_key_file" \
    '[.owners[] | select(.publicKeyFile == $file) | .namespace][0] // empty' "$GG_ARTIFACTS" | head -n 1)"
  if [[ -z "$fingerprint" && -f "$LOCALNET_STATE_DIR/extra-key-fingerprints.json" ]]; then
    fingerprint="$(jq -er --arg file "$public_key_file" 'if type == "object" then (.[$file] // "") else error("not a JSON object") end' \
      "$LOCALNET_STATE_DIR/extra-key-fingerprints.json")" \
      || { echo "$LOCALNET_STATE_DIR/extra-key-fingerprints.json is damaged" >&2; return 1; }
  fi
  if [[ -z "$fingerprint" ]]; then
    echo "No fingerprint known for $public_key_file" >&2
    return 1
  fi
  jq -n --arg keyFile "${public_key_file%.pub}.key" --arg fingerprint "$fingerprint" \
    '{keyFile: $keyFile, fingerprint: $fingerprint}'
}

# namespace_party_signers_json <party-id> <owner-name>... : signatures of owner keys under gg-keys/ for one
# party of the guardian decentralized namespace.
namespace_party_signers_json() {
  local party_id="$1" owner_name signers="[]"
  shift
  for owner_name in "$@"; do
    signers="$(jq --argjson signer "$(build_signer_json "$LOCALNET_STATE_DIR/gg-keys/$owner_name.pub")" '. + [$signer]' <<<"$signers")"
  done
  jq -n --arg party "$party_id" --argjson signers "$signers" '{($party): $signers}'
}

# gg_signers_json <owner-name>... : signatures of guardianGovernance owner keys under gg-keys/.
gg_signers_json() {
  namespace_party_signers_json "$GG_PARTY" "$@"
}

# Console prelude of the owner-key scripts: canton_console.sh "$OWNER_SIGNING_PRELUDE" <script>.
# shellcheck disable=SC2034 # used by the scripts that source this file
readonly OWNER_SIGNING_PRELUDE="$LOCALNET_SCRIPT_DIR/console/owner_signing_prelude.canton"
# Single-signatory probe of the vetting probe package. Its field `guardianGovernance` holds the signatory party.
readonly CEREMONY_TEMPLATE="#wormhole-localnet-vetting-probe:VettingProbe:GovernanceCeremonyProbe"

# ceremony_probe_create_command <signatory-party-id> <label>: CreateCommand JSON of one GovernanceCeremonyProbe.
ceremony_probe_create_command() {
  jq -n --arg templateId "$CEREMONY_TEMPLATE" --arg signatory "$1" --arg label "$2" \
    '{CreateCommand: {templateId: $templateId, createArguments: {guardianGovernance: $signatory, label: $label}}}'
}

# create_ceremony_probe <party-id> <label> <owner-name>...: that party creates a probe as its only signatory,
# signed by the given owner keys. Prints the transaction JSON.
create_ceremony_probe() {
  local party_id="$1" label="$2"
  shift 2
  submit_guardian_ceremony "ceremony-$label-$(date +%s)" "$(jq -cn --arg party "$party_id" '[$party]')" \
    "$(ceremony_probe_create_command "$party_id" "$label")" "$(namespace_party_signers_json "$party_id" "$@")"
}

# Custody parties under the guardian decentralized namespace (custody_host.sh). Each is empty until it exists.
# nttCustody owns the NTT reserves. feeCustody co-signs CoreState and receives the core fees.
CUSTODY_ARTIFACTS="$LOCALNET_STATE_DIR/$(custody_artifacts_file_name nttCustody)"
if ! CUSTODY_PARTY="$(read_custody_party_id "$CUSTODY_ARTIFACTS")"; then
  exit 1
fi
FEE_CUSTODY_ARTIFACTS="$LOCALNET_STATE_DIR/$(custody_artifacts_file_name feeCustody)"
if ! FEE_CUSTODY_PARTY="$(read_custody_party_id "$FEE_CUSTODY_ARTIFACTS")"; then
  exit 1
fi
# shellcheck disable=SC2034 # used by the scripts that source this file
readonly CUSTODY_PARTY FEE_CUSTODY_PARTY

# custody_party_id_of <party-name>: id of the custody party with that name, or empty until custody_host.sh ran.
custody_party_id_of() {
  case "$1" in
    nttCustody) echo "$CUSTODY_PARTY" ;;
    feeCustody) echo "$FEE_CUSTODY_PARTY" ;;
    *)
      echo "unknown custody party name: $1" >&2
      return 2
      ;;
  esac
}

# submit_interactive <command-id> <act-as-json-array> <command-json> <signers-json> [disclosed-json] [package-preference-json]
# Prints the transaction JSON. Returns non-zero on rejection; the error text goes to stderr.
# With SUBMIT_AWAIT_COMPLETION=true, prints the command completion (status included) instead.
submit_interactive() {
  local command_id="$1" act_as="$2" command="$3" signers="$4"
  local disclosed="${5:-[]}" package_preference="${6:-[]}" await_completion="${SUBMIT_AWAIT_COMPLETION:-false}"
  local spec_file="$gg_work_dir/$command_id.spec.json"
  jq -n --arg commandId "$command_id" --argjson actAs "$act_as" --argjson command "$command" \
    --argjson signers "$signers" --argjson disclosed "$disclosed" --argjson preference "$package_preference" \
    --arg synchronizerId "$LOGICAL_SYNCHRONIZER_ID" --argjson awaitCompletion "$await_completion" \
    '{commandId: $commandId, actAs: $actAs, commands: [$command], signers: $signers,
      disclosedContracts: $disclosed, packageIdSelectionPreference: $preference, synchronizerId: $synchronizerId,
      awaitCompletion: $awaitCompletion}' \
    > "$spec_file"
  node "$LOCALNET_SCRIPT_DIR/interactive_submit.js" \
    "$(json_api_url "$SUBMITTING_VALIDATOR")" "$gg_work_dir/script.jwt" "$spec_file"
}

# submit_guardian_ceremony: submit_interactive, with the same arguments and output, for a submission that guardian
# namespace parties sign. Each commit appends one line to GG_CEREMONY_LOG.
submit_guardian_ceremony() {
  local command_id="$1" act_as="$2" ceremony_output
  ceremony_output="$(submit_interactive "$@")" || { [[ -z "$ceremony_output" ]] || printf '%s\n' "$ceremony_output"; return 1; }
  jq -cn --arg commandId "$command_id" --argjson actAs "$act_as" \
    --arg updateId "$(jq -r '.updateId // .completion.updateId // ""' <<<"$ceremony_output")" \
    --arg recordedAt "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
    '{commandId: $commandId, updateId: $updateId, actAs: $actAs, recordedAt: $recordedAt}' >> "$GG_CEREMONY_LOG"
  printf '%s\n' "$ceremony_output"
}

# gg_ceremony_count: the number of lines in GG_CEREMONY_LOG.
gg_ceremony_count() {
  if [[ ! -f "$GG_CEREMONY_LOG" ]]; then
    echo 0
    return 0
  fi
  wc -l < "$GG_CEREMONY_LOG" | tr -d ' '
}

# created_contract_id <transaction-json> <qualified-template-suffix>, e.g. ":Wormhole.Core.State:CoreState".
created_contract_id() {
  jq -er --arg suffix "$2" \
    '[.events[] | .CreatedEvent? // empty | select(.templateId | endswith($suffix)) | .contractId] | if length == 1 then .[0] else error("expected exactly one created \($suffix), found \(length)") end' \
    <<<"$1"
}

# core_contract_id <name> [validator]: the one active contract of guardianGovernance that the validator (default
# SUBMITTING_VALIDATOR) shows. Names: coreState, emitterRegistry, replayRootRegistry, and emitter (owner CorePublisher).
# A submission that commits late can replace each of them.
core_contract_id() {
  local name="$1" validator_name="${2:-$SUBMITTING_VALIDATOR}" template_suffix required_owner=""
  case "$name" in
    coreState) template_suffix=":Wormhole.Core.State:CoreState" ;;
    emitterRegistry) template_suffix=":Wormhole.Core.State:EmitterRegistry" ;;
    replayRootRegistry) template_suffix=":Wormhole.Core.State:ReplayRootRegistry" ;;
    emitter) template_suffix=":Wormhole.Core.State:Emitter"; required_owner="$CORE_PUBLISHER_PARTY" ;;
    *) echo "core_contract_id: unknown name $name" >&2; return 2 ;;
  esac
  active_contracts "$validator_name" "$GG_PARTY" "$template_suffix" \
    | jq -er --arg name "$name" --arg gg "$GG_PARTY" --arg owner "$required_owner" \
      '[.[] | select(.createArgument.guardianGovernance == $gg) | select($owner == "" or .createArgument.owner == $owner)]
       | if length == 1 then .[0].contractId else error("expected one active \($name), found \(length)") end'
}

# active_contracts <validator> <party> <qualified-template-suffix>: active contracts of that template that the party sees there.
active_contracts() {
  local validator_name="$1" party="$2" template_suffix="$3" ledger_end
  ledger_end="$(curl_bounded -sSf "$(json_api_url "$validator_name")/v2/state/ledger-end" -H "@$SCRIPT_AUTH_HEADER" | jq -er .offset)"
  curl_bounded -sSf -X POST "$(json_api_url "$validator_name")/v2/state/active-contracts" \
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

# check_connected_to_gg_synchronizer <json-api-url> <participant-label>: the participant reports a connection to the
# synchronizer of gg-artifacts.json.
check_connected_to_gg_synchronizer() {
  local connected_synchronizer_ids
  connected_synchronizer_ids="$(curl_bounded -sSf "$1/v2/state/connected-synchronizers?party=$GG_PARTY" -H "@$SCRIPT_AUTH_HEADER" \
    | jq -er '.connectedSynchronizers | map(.synchronizerId) | .[]')" || {
    echo "FAIL: $2 reports no connected synchronizer" >&2
    return 1
  }
  if ! grep -qxF -e "$LOGICAL_SYNCHRONIZER_ID" -e "$SYNCHRONIZER_ID" <<<"$connected_synchronizer_ids"; then
    echo "FAIL: $2 is connected to $(tr '\n' ' ' <<<"$connected_synchronizer_ids"), expected $LOGICAL_SYNCHRONIZER_ID" >&2
    return 1
  fi
}

# generate_vaa <canton_localnet_vaa flags...>: prints the JSON {vaaHex, digestHex, pubKeys} of a devnet-key VAA.
generate_vaa() {
  (cd "$REPO_ROOT/node" && GOFLAGS=-mod=readonly GOPROXY=off go run ./hack/canton_localnet_vaa "$@")
}

# Daml [(Int, Bytes)] public-key hints from generate_vaa output.
vaa_pub_keys_json() {
  jq -c '[.pubKeys[] | {_1: (.guardianIndex | tostring), _2: .publicKeyHex}]' <<<"$1"
}

# active_contracts_with_blobs <validator> <party> <qualified-template-suffix>: like active_contracts, with created event blobs.
active_contracts_with_blobs() {
  local validator_name="$1" party="$2" template_suffix="$3" ledger_end
  ledger_end="$(curl_bounded -sSf "$(json_api_url "$validator_name")/v2/state/ledger-end" -H "@$SCRIPT_AUTH_HEADER" | jq -er .offset)"
  curl_bounded --max-time 120 -sSf -X POST "$(json_api_url "$validator_name")/v2/state/active-contracts" \
    -H "@$SCRIPT_AUTH_HEADER" -H 'Content-Type: application/json' \
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

# gg_disclosure <qualified-template-suffix> <contract-id>: disclosure of a contract that guardianGovernance sees on
# SUBMITTING_VALIDATOR.
gg_disclosure() {
  disclosed_contract "$SUBMITTING_VALIDATOR" "$GG_PARTY" "$1" "$2"
}

# local_submission_json <command-id> <act-as-json-array> <command-json> <disclosed-json>: JsCommands of one command.
local_submission_json() {
  jq -n --arg commandId "$1" --argjson actAs "$2" --argjson command "$3" --argjson disclosed "$4" \
    --arg userId "$SCRIPT_USER_ID" --arg synchronizerId "$LOGICAL_SYNCHRONIZER_ID" \
    '{commands: [$command], commandId: $commandId, actAs: $actAs, readAs: [], userId: $userId,
      disclosedContracts: $disclosed, synchronizerId: $synchronizerId}'
}

# submit_local_response <validator> <command-id> <act-as-json-array> <command-json> [disclosed-json]
# Submits as local parties of that participant. Prints the response body, then the HTTP status on the last line.
submit_local_response() {
  local validator_name="$1" command_id="$2" act_as="$3" command="$4" disclosed="${5:-[]}"
  curl_bounded --max-time 120 -sS -X POST "$(json_api_url "$validator_name")/v2/commands/submit-and-wait-for-transaction" \
    -H "@$SCRIPT_AUTH_HEADER" -H 'Content-Type: application/json' \
    -w '\n%{http_code}' \
    -d "$(jq -n --argjson commands "$(local_submission_json "$command_id" "$act_as" "$command" "$disclosed")" --argjson actAs "$act_as" '{
      commands: $commands,
      transactionFormat: {
        eventFormat: {filtersByParty: ($actAs | map({key: ., value: {cumulative: [{identifierFilter: {WildcardFilter: {value: {includeCreatedEventBlob: false}}}}]}}) | from_entries), verbose: true},
        transactionShape: "TRANSACTION_SHAPE_LEDGER_EFFECTS"}}')"
}

readonly LOCAL_COMPLETION_POLL_MAX_ATTEMPTS=120
# submit_local_and_await_completion <validator> <command-id> <act-as-json-array> <command-json> [disclosed-json]
# Asynchronous submission, then the command completion, so a synchronizer verdict such as a mediator timeout is
# visible. Prints {elapsedMs, completion}. Returns non-zero unless the completion is a success.
submit_local_and_await_completion() {
  local validator_name="$1" command_id="$2" act_as="$3" command="$4" disclosed="${5:-[]}"
  local submission_id="$2-submission" ledger_end started_seconds attempt completion
  ledger_end="$(curl_bounded -sSf "$(json_api_url "$validator_name")/v2/state/ledger-end" -H "@$SCRIPT_AUTH_HEADER" | jq -er .offset)"
  started_seconds="$(date +%s)"
  curl_bounded -sSf -X POST "$(json_api_url "$validator_name")/v2/commands/async/submit" -H "@$SCRIPT_AUTH_HEADER" \
    -H 'Content-Type: application/json' \
    -d "$(local_submission_json "$command_id" "$act_as" "$command" "$disclosed" | jq -c --arg id "$submission_id" '. + {submissionId: $id}')" \
    > /dev/null || return 1
  for ((attempt = 0; attempt < LOCAL_COMPLETION_POLL_MAX_ATTEMPTS; attempt++)); do
    completion="$(curl_bounded --max-time 60 -sSf -X POST \
      "$(json_api_url "$validator_name")/v2/commands/completions?stream_idle_timeout_ms=5000" \
      -H "@$SCRIPT_AUTH_HEADER" -H 'Content-Type: application/json' \
      -d "$(jq -n --argjson parties "$act_as" --argjson begin "$ledger_end" --arg userId "$SCRIPT_USER_ID" \
        '{userId: $userId, parties: $parties, beginExclusive: $begin}')" \
      | jq -c --arg id "$submission_id" '[.[] | .completionResponse.Completion?.value // empty | select(.submissionId == $id)] | first // empty')" \
      || return 1
    if [[ -n "$completion" ]]; then
      jq -cn --argjson completion "$completion" --argjson elapsedMs "$(( ($(date +%s) - started_seconds) * 1000 ))" \
        '{elapsedMs: $elapsedMs, completion: $completion}'
      [[ "$(jq -r '.status.code // 0' <<<"$completion")" == "0" ]]
      return
    fi
  done
  echo "no completion for $submission_id after $LOCAL_COMPLETION_POLL_MAX_ATTEMPTS polls" >&2
  return 1
}

# submit_local <validator> <command-id> <act-as-json-array> <command-json> [disclosed-json]
# Submits as local parties of that participant. Prints the transaction JSON.
# With SUBMIT_AWAIT_COMPLETION=true, prints the command completion of submit_local_and_await_completion instead.
submit_local() {
  local validator_name="$1" command_id="$2" response
  if [[ "${SUBMIT_AWAIT_COMPLETION:-false}" == true ]]; then
    submit_local_and_await_completion "$@"
    return
  fi
  response="$(submit_local_response "$@")" || return 1
  if [[ "$(tail -n 1 <<<"$response")" != "200" ]]; then
    echo "submit $command_id on $validator_name failed: $(sed '$d' <<<"$response")" >&2
    return 1
  fi
  sed '$d' <<<"$response" | jq -c .transaction
}

# allocate_local_party <validator> <party-hint>: allocate a party on that participant and print its id.
allocate_local_party() {
  curl_bounded --max-time 120 -sSf -X POST "$(json_api_url "$1")/v2/parties" -H "@$ADMIN_AUTH_HEADER" \
    -H 'Content-Type: application/json' -d "$(jq -n --arg hint "$2" '{partyIdHint: $hint, identityProviderId: ""}')" \
    | jq -er .partyDetails.party
}

# core_state_disclosures_on <validator>: the disclosure array of the current CoreState, read as guardianGovernance there.
core_state_disclosures_on() {
  local core_state_cid
  core_state_cid="$(core_contract_id coreState "$1")" || return 1
  core_state_disclosures "$(disclosed_contract "$1" "$GG_PARTY" :Wormhole.Core.State:CoreState "$core_state_cid")" "$core_state_cid"
}

# verify_vaa <command-id> <vaa-hex> <pub-keys-json>: ParseAndVerifyVAA on the current CoreState by CoreVerifier.
# guardianGovernance is a CoreState signatory, so its hosts confirm this transaction.
verify_vaa() {
  submit_local "$CORE_ROLE_VALIDATOR" "$1" "$(jq -cn --arg party "$CORE_VERIFIER_PARTY" '[$party]')" \
    "$(exercise_command "$CORE_STATE_TEMPLATE" "$(core_contract_id coreState "$CORE_ROLE_VALIDATOR")" ParseAndVerifyVAA \
      "$(jq -n --arg party "$CORE_VERIFIER_PARTY" --arg vaa "$2" --argjson keys "$3" '{verifier: $party, encodedVAA: $vaa, pubKeys: $keys}')")" \
    "$(core_state_disclosures_on "$CORE_ROLE_VALIDATOR")"
}

# relay_governance_vaa <command-id> <vaa-hex> <pub-keys-json>: SubmitGovernanceVAA on the current CoreState by
# GovernanceRelayer.
relay_governance_vaa() {
  submit_local "$CORE_ROLE_VALIDATOR" "$1" "$(jq -cn --arg party "$GOVERNANCE_RELAYER_PARTY" '[$party]')" \
    "$(relay_governance_vaa_command "$(core_contract_id coreState "$CORE_ROLE_VALIDATOR")" "$GOVERNANCE_RELAYER_PARTY" "$2" "$3")" \
    "$(core_state_disclosures_on "$CORE_ROLE_VALIDATOR")"
}

# publish_message <command-id> <payload-hex>: PublishMessage on the active CorePublisher Emitter at fee 0.
publish_message() {
  submit_local "$CORE_ROLE_VALIDATOR" "$1" "$(jq -cn --arg party "$CORE_PUBLISHER_PARTY" '[$party]')" \
    "$(exercise_command "$EMITTER_TEMPLATE" "$(core_contract_id emitter "$CORE_ROLE_VALIDATOR")" PublishMessage \
      "$(jq -n --arg party "$CORE_PUBLISHER_PARTY" --arg cs "$(core_contract_id coreState "$CORE_ROLE_VALIDATOR")" --arg payload "$2" \
        '{nonce: "2", payload: $payload, consistencyLevel: "0", coreStateCid: $cs, payer: $party, feeAllocation: null}')")" \
    "$(core_state_disclosures_on "$CORE_ROLE_VALIDATOR")"
}

# local_party_is_on <validator> <party-id>: prints true when the party is local to that participant, else false.
local_party_is_on() {
  curl_bounded -sS "$(json_api_url "$1")/v2/parties/$2" -H "@$ADMIN_AUTH_HEADER" \
    | jq -r --arg party "$2" '[.partyDetails[]? | select(.party == $party) | .isLocal] | first // false'
}

# check_party_is_local_on <party-id> <validator>
check_party_is_local_on() {
  [[ "$(local_party_is_on "$2" "$1")" == true ]] || fail "$1 is not a local party of $2"
}

# check_party_is_not_local_on <party-id> <validator>
check_party_is_not_local_on() {
  [[ "$(local_party_is_on "$2" "$1")" == false ]] || fail "$1 is a local party of $2"
}

# check_packages_vetted_on <validator> <package-id>...: the participant vets each package.
check_packages_vetted_on() {
  local validator_name="$1" participant_id vetted_ids package_id
  shift
  participant_id="$(curl_bounded -sSf "$(json_api_url "$validator_name")/v2/parties/participant-id" -H "@$ADMIN_AUTH_HEADER" | jq -er .participantId)"
  vetted_ids="$(curl_bounded --max-time 120 -sSf -X POST "$(json_api_url "$validator_name")/v2/package-vetting/list" -H "@$ADMIN_AUTH_HEADER" \
    -H 'Content-Type: application/json' -d "$(jq -n --arg pid "$participant_id" '{topologyStateFilter: {participantIds: [$pid]}}')" \
    | jq -r '.vettedPackages[]?.packages[].packageId' | sort -u)"
  for package_id in "$@"; do
    grep -qxF "$package_id" <<<"$vetted_ids" || fail "$validator_name does not vet $package_id"
  done
  echo "  $validator_name vets $(printf '%s\n' "$@" | cut -c1-12 | tr '\n' ' ')"
}

# completion_summary <completion-output-file>: elapsed time, status code and message of an awaited completion.
completion_summary() {
  jq -c '{elapsedMs, code: (.completion.status.code // 0), message: (.completion.status.message // ""),
          unresponsiveParties: ([.completion.status.details[]?.valueDecoded.metadata.unresponsiveParties? // empty] | first)}' "$1"
}

# start_clock_ticker <seconds> [participant]: run the synchronizer clock ticker in the background with time-proof
# requests from that participant (default app-provider); sets clock_ticker_pid.
start_clock_ticker() {
  CLOCK_TICKER_SECONDS="$1" CLOCK_TICKER_PARTICIPANT="${2:-app-provider}" CANTON_CONSOLE_TIMEOUT_SECONDS="$(($1 + 120))" \
    "$LOCALNET_SCRIPT_DIR/canton_console.sh" \
    "$LOCALNET_SCRIPT_DIR/console/synchronizer_clock_ticker.canton" > "$gg_work_dir/clock-ticker.log" 2>&1 &
  clock_ticker_pid=$!
}

# stop_clock_ticker: stops canton_console.sh and the process groups of its children (java and the watchdog).
stop_clock_ticker() {
  local ticker_child_pid
  if [[ -n "${clock_ticker_pid:-}" ]]; then
    for ticker_child_pid in $(pgrep -P "$clock_ticker_pid"); do
      kill -TERM -- "-$ticker_child_pid" 2>/dev/null || kill -TERM "$ticker_child_pid" 2>/dev/null || true
    done
    kill "$clock_ticker_pid" 2>/dev/null || true
    wait "$clock_ticker_pid" 2>/dev/null || true
    clock_ticker_pid=""
  fi
}
