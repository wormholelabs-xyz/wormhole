#!/usr/bin/env bash
#
# Genesis on the devnet sandbox with the guardianGovernance external party of
# setup_guardian_governance.sh. The operator is a local party of the
# participant (LabsOperator). It creates one WormholeGenesisOffer. Then
# guardianGovernance, feeCustody and nttCustody accept it in one prepared
# AcceptGenesis transaction, which the owner keys sign. That transaction
# creates the CoreState, the EmitterRegistry, the ReplayRootRegistry, the
# NttGovernance root and the gg CoinFactory.
#
# The guardian side compares the offer with its own agreed parameters before it
# signs (accept_genesis_arguments of canton/localnet/gg_offline_lib.sh).
# A rerun reuses an active offer. When a CoreState of guardianGovernance is
# active, the script prints "exists" and stops. Prints
# "RESULT WormholeGenesisOffer created|exists" and
# "RESULT AcceptGenesis committed|rejected|exists".
#
# Usage:
#   genesis_guardian_governance.sh <mode>
#
#   mode: owner signatures of guardianGovernance, feeCustody and nttCustody
#     happy                  threshold, threshold, threshold. Expected: committed.
#     threshold              1, threshold, threshold. Expected: rejected.
#     forge                  0, 0, 0 (the operator alone). Expected: rejected
#                            (the operator cannot make the guardian-anchored roots).
#     fee-custody-missing    threshold, 0, threshold. Expected: rejected.
#     fee-custody-threshold  threshold, 1, threshold. Expected: rejected.
#     ntt-custody-missing    threshold, threshold, 0. Expected: rejected.
#
# Required environment:
#   GG_ARTIFACTS_FILE       Artifacts JSON from setup_guardian_governance.sh,
#                           with feeCustodyPartyId and nttCustodyPartyId.
#   CANTON_HOST / CANTON_JSON_API_PORT
#                           Default localhost / 6864 (sandbox default).
#
# Optional environment:
#   GENESIS_GUARDIAN_SET_INDEX
#                           Global index of the genesis guardian set
#                           (default 0).
#   CURL_MAX_TIME_SECONDS / CURL_CONNECT_TIMEOUT_SECONDS
#                           Time limits of each JSON Ledger API call.
#                           Default 60 / 10.
set -euo pipefail

MODE="${1:?usage: genesis_guardian_governance.sh <happy|threshold|forge|fee-custody-missing|fee-custody-threshold|ntt-custody-missing>}"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../localnet/gg_offline_lib.sh
source "${SCRIPT_DIR}/../localnet/gg_offline_lib.sh"

HOST="${CANTON_HOST:-localhost}"
JSON_PORT="${CANTON_JSON_API_PORT:-6864}"
BASE_URL="http://${HOST}:${JSON_PORT}"
CURL_MAX_TIME_SECONDS="${CURL_MAX_TIME_SECONDS:-60}"
CURL_CONNECT_TIMEOUT_SECONDS="${CURL_CONNECT_TIMEOUT_SECONDS:-10}"
readonly USER_ID="participant_admin"
readonly DEVNET_GUARDIAN_ADDRESS="befa429d57cd18b7f8a4d91a2da9ab4af05d0fbe"
readonly GENESIS_GUARDIAN_SET_INDEX="${GENESIS_GUARDIAN_SET_INDEX:-0}"

: "${GG_ARTIFACTS_FILE:?GG_ARTIFACTS_FILE is required}"
GG_PARTY="$(jq -er '.partyId' "${GG_ARTIFACTS_FILE}")"
FEE_CUSTODY_PARTY="$(jq -er '.feeCustodyPartyId | strings' "${GG_ARTIFACTS_FILE}")" || {
  echo "${GG_ARTIFACTS_FILE} has no feeCustodyPartyId; run setup_guardian_governance.sh with GG_FEE_CUSTODY_PARTY_NAME=feeCustody" >&2
  exit 1
}
NTT_CUSTODY_PARTY="$(jq -er '.nttCustodyPartyId | strings' "${GG_ARTIFACTS_FILE}")" || {
  echo "${GG_ARTIFACTS_FILE} has no nttCustodyPartyId; run setup_guardian_governance.sh with GG_NTT_CUSTODY_PARTY_NAME=nttCustody" >&2
  exit 1
}
for custody_party in "${FEE_CUSTODY_PARTY}" "${NTT_CUSTODY_PARTY}"; do
  if [ "${custody_party#*::}" != "${GG_PARTY#*::}" ]; then
    echo "${custody_party} is outside the namespace of ${GG_PARTY}" >&2
    exit 1
  fi
done
GG_THRESHOLD_VALUE="$(jq -er '.threshold' "${GG_ARTIFACTS_FILE}")"
GG_OWNER_COUNT="$(jq '.owners | length' "${GG_ARTIFACTS_FILE}")"
SYNCHRONIZER_ID="$(jq -er '.synchronizerId' "${GG_ARTIFACTS_FILE}")"
# The JSON Ledger API takes the logical synchronizer id: strip "::<protocolVersion>-<serial>".
LOGICAL_SYNCHRONIZER_ID="$(echo "${SYNCHRONIZER_ID}" | sed -E 's/(::[^:]+)::[0-9]+-[0-9]+$/\1/')"

# namespace_party_signer_count <guardianGovernance|feeCustody|nttCustody>: owner signatures that MODE prescribes.
namespace_party_signer_count() {
  case "${MODE}:$1" in
    happy:*) echo "${GG_THRESHOLD_VALUE}" ;;
    threshold:guardianGovernance) echo 1 ;;
    threshold:*) echo "${GG_THRESHOLD_VALUE}" ;;
    forge:*) echo 0 ;;
    fee-custody-missing:feeCustody) echo 0 ;;
    fee-custody-threshold:feeCustody) echo 1 ;;
    ntt-custody-missing:nttCustody) echo 0 ;;
    fee-custody-missing:* | fee-custody-threshold:* | ntt-custody-missing:*) echo "${GG_THRESHOLD_VALUE}" ;;
    *) echo "unknown mode or party: ${MODE}:$1" >&2; return 2 ;;
  esac
}
namespace_party_signer_count guardianGovernance >/dev/null || exit 2

# api <GET|POST> <path> [json-body]: one bounded JSON Ledger API call; fails on an HTTP error.
api() {
  local method="$1" path="$2"
  if [ "${method}" = GET ]; then
    curl -sS --fail-with-body --max-time "${CURL_MAX_TIME_SECONDS}" --connect-timeout "${CURL_CONNECT_TIMEOUT_SECONDS}" \
      "${BASE_URL}${path}"
  else
    curl -sS --fail-with-body --max-time "${CURL_MAX_TIME_SECONDS}" --connect-timeout "${CURL_CONNECT_TIMEOUT_SECONDS}" \
      -X POST "${BASE_URL}${path}" -H 'Content-Type: application/json' -d "$3"
  fi
}

# labs_operator_party: the local party LabsOperator of this participant; allocated on the first run.
labs_operator_party() {
  local party_id
  party_id="$(api GET "/v2/parties?pageSize=1000" | jq -r '[.partyDetails[] | select(.isLocal) | .party | select(startswith("LabsOperator::"))] | first // empty')"
  if [ -z "${party_id}" ]; then
    party_id="$(api POST /v2/parties '{"partyIdHint": "LabsOperator", "identityProviderId": ""}' | jq -er .partyDetails.party)"
  fi
  api POST "/v2/users/${USER_ID}/rights" "$(jq -n --arg user "${USER_ID}" --arg party "${party_id}" \
    '{userId: $user, rights: [{kind: {CanActAs: {value: {party: $party}}}}], identityProviderId: ""}')" > /dev/null
  echo "${party_id}"
}

# active_created_events <party> <template-id>: active contracts of that template that the party sees.
active_created_events() {
  local ledger_end
  ledger_end="$(api GET /v2/state/ledger-end | jq -er .offset)"
  api POST /v2/state/active-contracts "$(jq -n --argjson end "${ledger_end}" --arg party "$1" --arg templateId "$2" '{
    activeAtOffset: $end,
    eventFormat: {filtersByParty: {($party): {cumulative: [
      {identifierFilter: {TemplateFilter: {value: {templateId: $templateId, includeCreatedEventBlob: false}}}}]}}, verbose: false}}')" \
    | jq -c '[.[] | .contractEntry.JsActiveContract?.createdEvent // empty]'
}

OPERATOR_PARTY="$(labs_operator_party)"
echo "[genesis:${MODE}] operator ${OPERATOR_PARTY}" >&2
if [ "$(active_created_events "${GG_PARTY}" "#wormhole-core:Wormhole.Core.State:CoreState" \
    | jq --arg gg "${GG_PARTY}" '[.[] | select(.createArgument.guardianGovernance == $gg)] | length')" != 0 ]; then
  echo "RESULT AcceptGenesis exists"
  exit 0
fi

# Fees stay 0 on devnet, so the fee instrument is a placeholder. The CoreState ensure rejects gg and feeCustody as its admin.
PLACEHOLDER_FEE_INSTRUMENT_ADMIN="UnusedFeeInstrumentAdmin::${OPERATOR_PARTY#*::}"
AGREED_PARAMETERS="$(first_genesis_parameters "${GG_PARTY}" "${FEE_CUSTODY_PARTY}" "${NTT_CUSTODY_PARTY}" "${OPERATOR_PARTY}" \
  "${PLACEHOLDER_FEE_INSTRUMENT_ADMIN}" UNUSED "${GENESIS_GUARDIAN_SET_INDEX}" "${DEVNET_GUARDIAN_ADDRESS}")"

offers="$(active_created_events "${OPERATOR_PARTY}" "${GENESIS_OFFER_TEMPLATE}" \
  | jq -c --arg operator "${OPERATOR_PARTY}" '[.[] | select(.createArgument.operator == $operator)]')"
case "$(jq length <<<"${offers}")" in
  0)
    api POST /v2/commands/submit-and-wait "$(jq -n --arg commandId "genesis-offer-${MODE}-$$" --arg operator "${OPERATOR_PARTY}" \
      --arg user "${USER_ID}" --arg synchronizerId "${LOGICAL_SYNCHRONIZER_ID}" \
      --argjson command "$(create_command "${GENESIS_OFFER_TEMPLATE}" "$(genesis_offer_arguments "${OPERATOR_PARTY}" "${AGREED_PARAMETERS}")")" \
      '{commands: [$command], commandId: $commandId, actAs: [$operator], readAs: [], userId: $user, synchronizerId: $synchronizerId}')" > /dev/null
    echo "RESULT WormholeGenesisOffer created"
    offers="$(active_created_events "${OPERATOR_PARTY}" "${GENESIS_OFFER_TEMPLATE}" \
      | jq -c --arg operator "${OPERATOR_PARTY}" '[.[] | select(.createArgument.operator == $operator)]')"
    ;;
  1) echo "RESULT WormholeGenesisOffer exists" ;;
  *) echo "[genesis:${MODE}] $(jq length <<<"${offers}") active offers of ${OPERATOR_PARTY}, expected at most 1" >&2; exit 1 ;;
esac
OFFER_EVENT="$(jq -ec 'if length == 1 then .[0] else error("expected one active offer") end' <<<"${offers}")"
ACCEPT_ARGUMENTS="$(accept_genesis_arguments "$(jq -c .createArgument <<<"${OFFER_EVENT}")" "${AGREED_PARAMETERS}")"

sign_hash() {
  node "${SCRIPT_DIR}/guardian_key_tool.js" sign "$1" "$2"
}
hex_to_b64() {
  python3 -c 'import sys, base64; sys.stdout.write(base64.b64encode(bytes.fromhex(sys.argv[1])).decode())' "$1"
}
b64_to_hex() {
  python3 -c 'import sys, base64; sys.stdout.write(base64.b64decode(sys.argv[1]).hex())' "$1"
}

# namespace_party_signatures_json <hash-hex> <signer-count>: signatures of the first owner keys of the guardian
# namespace. The three parties of the namespace have the same signing keys.
namespace_party_signatures_json() {
  local hash_hex="$1" signer_count="$2" signatures="[]" owner_position fingerprint public_key_file signature_hex
  for ((owner_position = 0; owner_position < signer_count; owner_position++)); do
    fingerprint="$(jq -er ".owners[${owner_position}].namespace" "${GG_ARTIFACTS_FILE}")"
    public_key_file="$(jq -er ".owners[${owner_position}].publicKeyFile" "${GG_ARTIFACTS_FILE}")"
    signature_hex="$(sign_hash "${public_key_file%.pub}.key" "${hash_hex}")"
    signatures="$(jq --arg sig "$(hex_to_b64 "${signature_hex}")" --arg fp "${fingerprint}" \
      '. + [{format: "SIGNATURE_FORMAT_CONCAT", signature: $sig, signedBy: $fp, signingAlgorithmSpec: "SIGNING_ALGORITHM_SPEC_ED25519"}]' \
      <<<"${signatures}")"
  done
  printf '%s' "${signatures}"
}

command_id="guardian-governance-genesis-accept-${MODE}-$$"
echo "[genesis:${MODE}] preparing AcceptGenesis on offer $(jq -r .contractId <<<"${OFFER_EVENT}" | cut -c1-16)..." >&2
if ! prepare_response="$(api POST /v2/interactive-submission/prepare "$(jq -n --arg commandId "${command_id}" \
    --arg gg "${GG_PARTY}" --arg fee "${FEE_CUSTODY_PARTY}" --arg ntt "${NTT_CUSTODY_PARTY}" --arg user "${USER_ID}" \
    --arg synchronizerId "${LOGICAL_SYNCHRONIZER_ID}" \
    --argjson command "$(exercise_command "${GENESIS_OFFER_TEMPLATE}" "$(jq -r .contractId <<<"${OFFER_EVENT}")" AcceptGenesis "${ACCEPT_ARGUMENTS}")" \
    '{commandId: $commandId, actAs: [$gg, $fee, $ntt], readAs: [], synchronizerId: $synchronizerId, packageIdSelectionPreference: [],
      verboseHashing: false, userId: $user, commands: [$command]}')")"; then
  echo "[genesis:${MODE}] prepare of AcceptGenesis failed: ${prepare_response}" >&2
  echo "RESULT AcceptGenesis rejected"
  exit 1
fi
hash_hex="$(b64_to_hex "$(jq -er '.preparedTransactionHash' <<<"${prepare_response}")")"
if [ "${#hash_hex}" -ne 64 ]; then
  echo "[genesis:${MODE}] prepared hash has ${#hash_hex} hex characters, expected 64" >&2
  exit 1
fi
party_signatures="$(jq -n \
  --arg gg "${GG_PARTY}" --argjson ggSig "$(namespace_party_signatures_json "${hash_hex}" "$(namespace_party_signer_count guardianGovernance)")" \
  --arg fee "${FEE_CUSTODY_PARTY}" --argjson feeSig "$(namespace_party_signatures_json "${hash_hex}" "$(namespace_party_signer_count feeCustody)")" \
  --arg ntt "${NTT_CUSTODY_PARTY}" --argjson nttSig "$(namespace_party_signatures_json "${hash_hex}" "$(namespace_party_signer_count nttCustody)")" \
  '{signatures: ([{party: $gg, signatures: $ggSig}, {party: $fee, signatures: $feeSig}, {party: $ntt, signatures: $nttSig}]
     | map(select(.signatures | length > 0)))}')"
echo "[genesis:${MODE}] executing AcceptGenesis with $(namespace_party_signer_count guardianGovernance)/$(namespace_party_signer_count feeCustody)/$(namespace_party_signer_count nttCustody)-of-${GG_OWNER_COUNT} guardianGovernance/feeCustody/nttCustody signatures (threshold ${GG_THRESHOLD_VALUE})..." >&2
execute_response_file="$(mktemp)"
trap 'rm -f "${execute_response_file}"' EXIT
http_code="$(curl -s --max-time "${CURL_MAX_TIME_SECONDS}" --connect-timeout "${CURL_CONNECT_TIMEOUT_SECONDS}" \
  -o "${execute_response_file}" -w '%{http_code}' -X POST "${BASE_URL}/v2/interactive-submission/executeAndWait" \
  -H 'Content-Type: application/json' -d "$(jq -n --arg prepared "$(jq -er .preparedTransaction <<<"${prepare_response}")" \
    --argjson partySignatures "${party_signatures}" --arg submissionId "${command_id}-exec" --arg user "${USER_ID}" \
    --arg hashingSchemeVersion "$(jq -er .hashingSchemeVersion <<<"${prepare_response}")" \
    '{preparedTransaction: $prepared, partySignatures: $partySignatures, submissionId: $submissionId, userId: $user,
      hashingSchemeVersion: $hashingSchemeVersion, deduplicationPeriod: {Empty: {}}}')")" || http_code="curl-failed"
echo "[genesis:${MODE}] AcceptGenesis: HTTP ${http_code}" >&2
cat "${execute_response_file}" >&2
echo >&2
if [ "${http_code}" = "200" ]; then
  echo "RESULT AcceptGenesis committed"
  exit 0
fi
echo "RESULT AcceptGenesis rejected"
exit 1
