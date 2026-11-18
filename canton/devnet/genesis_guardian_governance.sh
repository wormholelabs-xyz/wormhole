#!/usr/bin/env bash
#
# Demonstrate that the guardianGovernance external party, bootstrapped by
# setup_guardian_governance.sh, can actually co-sign a transaction: prepare the
# genesis `create CoreState` command (co-signed by an external `operator`
# party, guardianGovernance and feeCustody, the fee recipient) and
# `create EmitterRegistry` command (co-signed by operator and
# guardianGovernance), mirroring Test.TestCore:setup but via interactive
# submission instead of Daml Script, have owners sign the prepared hash, and
# execute. The prepare endpoint takes one command, so each create is its own
# prepared transaction. CoreState is created first; when it is not committed,
# the script exits without the EmitterRegistry create. A create whose contract
# is already active for the (operator, guardianGovernance) pair is skipped, so
# a rerun creates only what is missing. Prints
# "RESULT <template> committed|rejected|exists" for each create it reaches.
#
# This is a THIN orchestration layer over the JSON Ledger API's interactive
# submission endpoints (prepare/execute) -- see
# https://docs.digitalasset.com/build/3.5/tutorials/app-dev/external_signing_submission.html
# for the underlying flow. jq/curl/node are already relied on elsewhere in
# this repo (see scripts/delegated-guardian-set-preset.sh); no new
# dependencies.
#
# Usage:
#   genesis_guardian_governance.sh <mode>
#
#   mode: "happy" | "threshold" | "forge" | "fee-custody-missing" | "fee-custody-threshold"
#     happy      Operator (1-of-1 external key) + 2-of-3 guardianGovernance
#                and 2-of-3 feeCustody signatures. Expected: contract created.
#     threshold  Operator + only 1-of-3 guardianGovernance signatures.
#                Expected: rejected (threshold not met).
#     forge      Operator signs alone; guardianGovernance and feeCustody
#                provide NO signature at all. Expected: rejected (the
#                anti-forgery crux property -- the operator cannot fabricate
#                the guardian-anchored CoreState or EmitterRegistry).
#     fee-custody-missing
#                2-of-3 guardianGovernance signatures, no feeCustody
#                signature. Expected: CoreState rejected.
#     fee-custody-threshold
#                2-of-3 guardianGovernance signatures, 1-of-3 feeCustody
#                signatures. Expected: CoreState rejected.
#
# Required environment:
#   GG_ARTIFACTS_FILE       Artifacts JSON from setup_guardian_governance.sh,
#                           with feeCustodyPartyId.
#   GG_OPERATOR_ARTIFACTS_FILE
#                           Same-shaped artifacts JSON for a single-owner
#                           (1-of-1) external `operator` party (this script
#                           does not allocate one; see
#                           test_guardian_governance.sh for how to produce it
#                           by re-running guardian_governance.canton with
#                           GG_PARTY_NAME=Operator, GG_THRESHOLD=1 and a
#                           single owner key repeated, or a dedicated
#                           single-key setup).
#   GG_CORE_PACKAGE_ID      Package id of the uploaded wormhole-core DAR
#                           (see `dpm inspect-dar`).
#   CANTON_HOST / CANTON_LEDGER_API_PORT / CANTON_JSON_API_PORT
#                           Default localhost / 6865 / 6864 (sandbox
#                           defaults).
#
# Optional environment:
#   GENESIS_LAST_CREATE     CoreState or EmitterRegistry (default). The
#                           script stops after this create.
#   GENESIS_GUARDIAN_SET_INDEX
#                           Global index of the genesis guardian set
#                           (default 0).
#   CURL_MAX_TIME_SECONDS / CURL_CONNECT_TIMEOUT_SECONDS
#                           Time limits of each JSON Ledger API call.
#                           Default 60 / 10.
set -euo pipefail

MODE="${1:?usage: genesis_guardian_governance.sh <happy|threshold|forge|fee-custody-missing|fee-custody-threshold>}"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

HOST="${CANTON_HOST:-localhost}"
JSON_PORT="${CANTON_JSON_API_PORT:-6864}"
BASE_URL="http://${HOST}:${JSON_PORT}"
CURL_MAX_TIME_SECONDS="${CURL_MAX_TIME_SECONDS:-60}"
CURL_CONNECT_TIMEOUT_SECONDS="${CURL_CONNECT_TIMEOUT_SECONDS:-10}"
GENESIS_LAST_CREATE="${GENESIS_LAST_CREATE:-EmitterRegistry}"
case "${GENESIS_LAST_CREATE}" in
  CoreState | EmitterRegistry) ;;
  *) echo "GENESIS_LAST_CREATE must be CoreState or EmitterRegistry, got: ${GENESIS_LAST_CREATE}" >&2; exit 2 ;;
esac

: "${GG_ARTIFACTS_FILE:?GG_ARTIFACTS_FILE is required}"
: "${GG_OPERATOR_ARTIFACTS_FILE:?GG_OPERATOR_ARTIFACTS_FILE is required}"
: "${GG_CORE_PACKAGE_ID:?GG_CORE_PACKAGE_ID is required}"

GG_PARTY="$(jq -r '.partyId' "${GG_ARTIFACTS_FILE}")"
FEE_CUSTODY_PARTY="$(jq -er '.feeCustodyPartyId | strings' "${GG_ARTIFACTS_FILE}")" || {
  echo "${GG_ARTIFACTS_FILE} has no feeCustodyPartyId; run setup_guardian_governance.sh with GG_FEE_CUSTODY_PARTY_NAME=feeCustody" >&2
  exit 1
}
if [ "${FEE_CUSTODY_PARTY#*::}" != "${GG_PARTY#*::}" ]; then
  echo "feeCustody ${FEE_CUSTODY_PARTY} is outside the namespace of ${GG_PARTY}" >&2
  exit 1
fi
GG_THRESHOLD_VALUE="$(jq -r '.threshold' "${GG_ARTIFACTS_FILE}")"
SYNCHRONIZER_ID="$(jq -r '.synchronizerId' "${GG_ARTIFACTS_FILE}")"
# The JSON Ledger API's `synchronizerId` field on PrepareSubmissionRequest
# wants the LOGICAL synchronizer id ("<name>::<namespace-fingerprint>"), not
# the PHYSICAL id this repo's artifacts carry ("...::<protocol>-<serial>") --
# strip the trailing "::<protocolVersion>-<serial>" suffix.
LOGICAL_SYNCHRONIZER_ID="$(echo "${SYNCHRONIZER_ID}" | sed -E 's/(::[^:]+)::[0-9]+-[0-9]+$/\1/')"

OP_PARTY="$(jq -r '.partyId' "${GG_OPERATOR_ARTIFACTS_FILE}")"
OP_FINGERPRINT="$(jq -r '.owners[0].namespace' "${GG_OPERATOR_ARTIFACTS_FILE}")"
OP_KEYFILE_PUB="$(jq -r '.owners[0].publicKeyFile' "${GG_OPERATOR_ARTIFACTS_FILE}")"
OP_KEYFILE="${OP_KEYFILE_PUB%.pub}.key"

sign_hash() {
  local keyfile="$1" hash_hex="$2"
  node "${SCRIPT_DIR}/guardian_key_tool.js" sign "${keyfile}" "${hash_hex}"
}

hex_to_b64() {
  python3 -c 'import sys, base64; sys.stdout.write(base64.b64encode(bytes.fromhex(sys.argv[1])).decode())' "$1"
}

b64_to_hex() {
  python3 -c 'import sys, base64; sys.stdout.write(base64.b64decode(sys.argv[1]).hex())' "$1"
}

sig_json() {
  local sig_hex="$1" fingerprint="$2"
  local sig_b64
  sig_b64="$(hex_to_b64 "${sig_hex}")"
  jq -n --arg sig "${sig_b64}" --arg fp "${fingerprint}" \
    '{format: "SIGNATURE_FORMAT_CONCAT", signature: $sig, signedBy: $fp, signingAlgorithmSpec: "SIGNING_ALGORITHM_SPEC_ED25519"}'
}

# --- Build the genesis create commands, matching mkInitialCoreState's and
# mkInitialEmitterRegistry's devnet values (Test.TestCore.setup). ---
# The global guardian set index of the genesis set; the CoreState precondition requires 0 to 4294967295.
readonly GENESIS_GUARDIAN_SET_INDEX="${GENESIS_GUARDIAN_SET_INDEX:-0}"
if [[ ! "${GENESIS_GUARDIAN_SET_INDEX}" =~ ^(0|[1-9][0-9]{0,9})$ ]] || (( GENESIS_GUARDIAN_SET_INDEX > 4294967295 )); then
  echo "GENESIS_GUARDIAN_SET_INDEX must be a decimal integer from 0 to 4294967295, got: ${GENESIS_GUARDIAN_SET_INDEX}" >&2
  exit 1
fi
CORE_STATE_ARGS="$(jq -n \
  --arg operator "${OP_PARTY}" \
  --arg gg "${GG_PARTY}" \
  --arg guardianSetIndex "${GENESIS_GUARDIAN_SET_INDEX}" \
  --arg feeCustody "${FEE_CUSTODY_PARTY}" \
  '{
    operator: $operator,
    guardianGovernance: $gg,
    chainId: "75",
    governanceChainId: "1",
    governanceContract: "0000000000000000000000000000000000000000000000000000000000000004",
    guardianSetIndex: $guardianSetIndex,
    guardianSets: [[$guardianSetIndex, {"keys": ["befa429d57cd18b7f8a4d91a2da9ab4af05d0fbe"], "expirationTime": null}]],
    messageFee: "0",
    feeInstrument: {admin: $gg, id: "UNUSED"},
    feeRecipient: $feeCustody,
    consumedGovernance: {"map": []}
  }')"
EMITTER_REGISTRY_ARGS="$(jq -n \
  --arg operator "${OP_PARTY}" \
  --arg gg "${GG_PARTY}" \
  '{
    operator: $operator,
    guardianGovernance: $gg,
    registrationFee: "0",
    nextId: "0"
  }')"

GG_OWNER_COUNT="$(jq '.owners | length' "${GG_ARTIFACTS_FILE}")"
EXEC_RESPONSE_FILE="$(mktemp)"
trap 'rm -f "${EXEC_RESPONSE_FILE}"' EXIT

# namespace_party_signer_count <guardianGovernance|feeCustody>: owner signatures that MODE prescribes for the party.
namespace_party_signer_count() {
  case "${MODE}:$1" in
    happy:*) echo "${GG_THRESHOLD_VALUE}" ;;              # exactly meets the threshold
    threshold:guardianGovernance) echo 1 ;;               # below the threshold
    threshold:feeCustody) echo "${GG_THRESHOLD_VALUE}" ;;
    forge:*) echo 0 ;;                                    # the operator alone
    fee-custody-missing:guardianGovernance | fee-custody-threshold:guardianGovernance) echo "${GG_THRESHOLD_VALUE}" ;;
    fee-custody-missing:feeCustody) echo 0 ;;
    fee-custody-threshold:feeCustody) echo 1 ;;
    *) echo "unknown mode or party: ${MODE}:$1" >&2; return 2 ;;
  esac
}
namespace_party_signer_count guardianGovernance >/dev/null || exit 2

# namespace_party_signatures_json <hash-hex> <signer-count>: signatures of the first owner keys of the guardian
# namespace. guardianGovernance and feeCustody have the same signing keys.
namespace_party_signatures_json() {
  local hash_hex="$1" signer_count="$2" signatures="[]" owner_position fingerprint public_key_file signature_hex
  for ((owner_position = 0; owner_position < signer_count; owner_position++)); do
    fingerprint="$(jq -er ".owners[${owner_position}].namespace" "${GG_ARTIFACTS_FILE}")"
    public_key_file="$(jq -er ".owners[${owner_position}].publicKeyFile" "${GG_ARTIFACTS_FILE}")"
    signature_hex="$(sign_hash "${public_key_file%.pub}.key" "${hash_hex}")"
    signatures="$(jq --argjson signature "$(sig_json "${signature_hex}" "${fingerprint}")" '. + [$signature]' <<<"${signatures}")"
  done
  printf '%s' "${signatures}"
}

# submit_genesis_create <label> <template-id> <create-arguments-json>: one prepared transaction with one
# CreateCommand, actAs Operator and guardianGovernance, and feeCustody for CoreState. The prepare endpoint accepts
# one command only. Prints "RESULT <label> committed" or "RESULT <label> rejected". Returns non-zero unless committed.
submit_genesis_create() {
  local label="$1" template_id="$2" create_arguments="$3"
  local command_id="guardian-governance-genesis-${MODE}-${label}-$$"
  local prepare_request prepare_response prepared_transaction hash_hex hashing_scheme_version
  local operator_signature gg_signatures fee_custody_signatures party_signatures execute_request http_code
  local act_as_parties
  if [ "${label}" = "CoreState" ]; then
    act_as_parties="$(jq -cn --arg op "${OP_PARTY}" --arg gg "${GG_PARTY}" --arg feeCustody "${FEE_CUSTODY_PARTY}" '[$op, $gg, $feeCustody]')"
  else
    act_as_parties="$(jq -cn --arg op "${OP_PARTY}" --arg gg "${GG_PARTY}" '[$op, $gg]')"
  fi
  prepare_request="$(jq -n \
    --arg cmdId "${command_id}" \
    --argjson actAs "${act_as_parties}" \
    --arg synchronizerId "${LOGICAL_SYNCHRONIZER_ID}" \
    --arg templateId "${template_id}" \
    --argjson createArguments "${create_arguments}" \
    '{
      commandId: $cmdId,
      actAs: $actAs,
      synchronizerId: $synchronizerId,
      packageIdSelectionPreference: [],
      verboseHashing: false,
      hashingSchemeVersion: "HASHING_SCHEME_VERSION_V3",
      userId: "participant_admin",
      commands: [ { CreateCommand: { templateId: $templateId, createArguments: $createArguments } } ]
    }')"

  echo "[genesis:${MODE}] preparing the ${label} create command..." >&2
  if ! prepare_response="$(curl -sS --fail-with-body --max-time "${CURL_MAX_TIME_SECONDS}" \
      --connect-timeout "${CURL_CONNECT_TIMEOUT_SECONDS}" -X POST "${BASE_URL}/v2/interactive-submission/prepare" \
      -H 'Content-Type: application/json' -d "${prepare_request}")"; then
    echo "[genesis:${MODE}] prepare of ${label} failed: ${prepare_response}" >&2
    echo "RESULT ${label} rejected"
    return 1
  fi
  prepared_transaction="$(jq -er '.preparedTransaction' <<<"${prepare_response}")"
  hashing_scheme_version="$(jq -er '.hashingSchemeVersion' <<<"${prepare_response}")"
  hash_hex="$(b64_to_hex "$(jq -er '.preparedTransactionHash' <<<"${prepare_response}")")"
  if [ "${#hash_hex}" -ne 64 ]; then
    echo "[genesis:${MODE}] prepared hash of ${label} has ${#hash_hex} hex characters, expected 64" >&2
    return 1
  fi

  operator_signature="$(sig_json "$(sign_hash "${OP_KEYFILE}" "${hash_hex}")" "${OP_FINGERPRINT}")"
  gg_signatures="$(namespace_party_signatures_json "${hash_hex}" "$(namespace_party_signer_count guardianGovernance)")"
  fee_custody_signatures="[]"
  if [ "${label}" = "CoreState" ]; then
    fee_custody_signatures="$(namespace_party_signatures_json "${hash_hex}" "$(namespace_party_signer_count feeCustody)")"
  fi
  party_signatures="$(jq -n \
    --arg op "${OP_PARTY}" --argjson opSig "[${operator_signature}]" \
    --arg gg "${GG_PARTY}" --argjson ggSig "${gg_signatures}" \
    --arg feeCustody "${FEE_CUSTODY_PARTY}" --argjson feeCustodySig "${fee_custody_signatures}" \
    '{ signatures: ([ {party: $op, signatures: $opSig}, {party: $gg, signatures: $ggSig},
                      {party: $feeCustody, signatures: $feeCustodySig} ]
                    | map(select(.signatures | length > 0))) }')"
  execute_request="$(jq -n \
    --arg prepared "${prepared_transaction}" \
    --argjson partySignatures "${party_signatures}" \
    --arg submissionId "${command_id}-exec" \
    --arg hsv "${hashing_scheme_version}" \
    '{
      preparedTransaction: $prepared,
      partySignatures: $partySignatures,
      submissionId: $submissionId,
      userId: "participant_admin",
      hashingSchemeVersion: $hsv,
      deduplicationPeriod: { Empty: {} }
    }')"

  echo "[genesis:${MODE}] executing ${label} with $(jq 'length' <<<"${gg_signatures}")-of-${GG_OWNER_COUNT} guardianGovernance and $(jq 'length' <<<"${fee_custody_signatures}")-of-${GG_OWNER_COUNT} feeCustody signature(s) (threshold ${GG_THRESHOLD_VALUE})..." >&2
  http_code="$(curl -s --max-time "${CURL_MAX_TIME_SECONDS}" --connect-timeout "${CURL_CONNECT_TIMEOUT_SECONDS}" \
    -o "${EXEC_RESPONSE_FILE}" -w '%{http_code}' -X POST \
    "${BASE_URL}/v2/interactive-submission/executeAndWait" -H 'Content-Type: application/json' -d "${execute_request}")" \
    || http_code="curl-failed"
  echo "[genesis:${MODE}] ${label}: HTTP ${http_code}" >&2
  cat "${EXEC_RESPONSE_FILE}" >&2
  echo >&2
  if [ "${http_code}" = "200" ]; then
    echo "RESULT ${label} committed"
    return 0
  fi
  echo "RESULT ${label} rejected"
  return 1
}

# count_active_genesis_contracts <template>: active wormhole-core contracts of the template for the
# (operator, guardianGovernance) pair, of any package version. Read as guardianGovernance.
count_active_genesis_contracts() {
  local template="$1" ledger_end active_contracts
  ledger_end="$(curl -sS --fail-with-body --max-time "${CURL_MAX_TIME_SECONDS}" \
    --connect-timeout "${CURL_CONNECT_TIMEOUT_SECONDS}" "${BASE_URL}/v2/state/ledger-end" | jq -er .offset)" || return 1
  active_contracts="$(curl -sS --fail-with-body --max-time "${CURL_MAX_TIME_SECONDS}" \
    --connect-timeout "${CURL_CONNECT_TIMEOUT_SECONDS}" -X POST "${BASE_URL}/v2/state/active-contracts" \
    -H 'Content-Type: application/json' \
    -d "$(jq -n --argjson end "${ledger_end}" --arg gg "${GG_PARTY}" \
      --arg templateId "#wormhole-core:Wormhole.Core.State:${template}" '{
      activeAtOffset: $end,
      eventFormat: {filtersByParty: {($gg): {cumulative: [
        {identifierFilter: {TemplateFilter: {value: {templateId: $templateId, includeCreatedEventBlob: false}}}}]}},
        verbose: false}}')")" || return 1
  jq -er --arg operator "${OP_PARTY}" --arg gg "${GG_PARTY}" '
    [.[] | .contractEntry.JsActiveContract?.createdEvent // empty
     | select(.packageName == "wormhole-core")
     | select(.createArgument.operator == $operator and .createArgument.guardianGovernance == $gg)]
    | length' <<<"${active_contracts}"
}

# create_genesis_contract_if_missing <label> <template-id> <create-arguments-json>: skips the create when one
# contract of the template is active for the pair. Returns non-zero when more than one is active, or when the
# create is not committed.
create_genesis_contract_if_missing() {
  local label="$1" active_count
  active_count="$(count_active_genesis_contracts "${label}")" || {
    echo "[genesis:${MODE}] the active ${label} query failed" >&2
    return 1
  }
  case "${active_count}" in
    0) submit_genesis_create "$@" ;;
    1) echo "RESULT ${label} exists" ;;
    *) echo "[genesis:${MODE}] ${active_count} active ${label} contracts for the pair, expected at most 1" >&2; return 1 ;;
  esac
}

create_genesis_contract_if_missing CoreState "${GG_CORE_PACKAGE_ID}:Wormhole.Core.State:CoreState" "${CORE_STATE_ARGS}" || {
  echo "[genesis:${MODE}] CoreState is not active; the EmitterRegistry create is not submitted" >&2
  exit 1
}
if [ "${GENESIS_LAST_CREATE}" = "CoreState" ]; then
  exit 0
fi
create_genesis_contract_if_missing EmitterRegistry "${GG_CORE_PACKAGE_ID}:Wormhole.Core.State:EmitterRegistry" "${EMITTER_REGISTRY_ARGS}"
