#!/usr/bin/env bash
#
# Demonstrate that the guardianGovernance external party, bootstrapped by
# setup_guardian_governance.sh, can actually co-sign a transaction: prepare the
# genesis `create CoreState` and `create EmitterRegistry` commands (both
# co-signed by an external `operator` party and guardianGovernance, mirroring
# Test.TestCore:setup's
# `submit (actAs operator <> actAs guardianGovernance)` but via interactive
# submission instead of Daml Script), have owners sign the prepared hash, and
# execute. The prepare endpoint takes one command, so each create is its own
# prepared transaction. Prints "RESULT <template> committed|rejected" for each.
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
#   mode: "happy" | "threshold" | "forge"
#     happy      Operator (1-of-1 external key) + 2-of-3 guardianGovernance
#                signatures. Expected: contract created.
#     threshold  Operator + only 1-of-3 guardianGovernance signatures.
#                Expected: rejected (threshold not met).
#     forge      Operator signs alone; guardianGovernance provides NO
#                signature at all. Expected: rejected (the anti-forgery
#                crux property -- the operator cannot fabricate the
#                guardian-anchored CoreState or EmitterRegistry).
#
# Required environment:
#   GG_ARTIFACTS_FILE       Artifacts JSON from setup_guardian_governance.sh.
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
set -euo pipefail

MODE="${1:?usage: genesis_guardian_governance.sh <happy|threshold|forge>}"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

HOST="${CANTON_HOST:-localhost}"
JSON_PORT="${CANTON_JSON_API_PORT:-6864}"
BASE_URL="http://${HOST}:${JSON_PORT}"

: "${GG_ARTIFACTS_FILE:?GG_ARTIFACTS_FILE is required}"
: "${GG_OPERATOR_ARTIFACTS_FILE:?GG_OPERATOR_ARTIFACTS_FILE is required}"
: "${GG_CORE_PACKAGE_ID:?GG_CORE_PACKAGE_ID is required}"

GG_PARTY="$(jq -r '.partyId' "${GG_ARTIFACTS_FILE}")"
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
CORE_STATE_ARGS="$(jq -n \
  --arg operator "${OP_PARTY}" \
  --arg gg "${GG_PARTY}" \
  '{
    operator: $operator,
    guardianGovernance: $gg,
    chainId: "75",
    governanceChainId: "1",
    governanceContract: "0000000000000000000000000000000000000000000000000000000000000004",
    guardianSetIndex: "0",
    guardianSets: [["0", {"keys": ["befa429d57cd18b7f8a4d91a2da9ab4af05d0fbe"], "expirationTime": null}]],
    messageFee: "0",
    feeInstrument: {admin: $gg, id: "UNUSED"},
    feeRecipient: $gg,
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

# gg_signatures_json <hash-hex>: the guardianGovernance owner signatures that MODE prescribes.
gg_signatures_json() {
  local hash_hex="$1" signatures="[]" owner_position fingerprint public_key_file signature_hex
  local signer_count
  case "${MODE}" in
    happy) signer_count="${GG_THRESHOLD_VALUE}" ;;  # exactly meets the threshold
    threshold) signer_count=1 ;;                    # below the threshold
    forge) signer_count=0 ;;                        # the operator alone
    *) echo "unknown mode: ${MODE}" >&2; return 2 ;;
  esac
  for ((owner_position = 0; owner_position < signer_count; owner_position++)); do
    fingerprint="$(jq -er ".owners[${owner_position}].namespace" "${GG_ARTIFACTS_FILE}")"
    public_key_file="$(jq -er ".owners[${owner_position}].publicKeyFile" "${GG_ARTIFACTS_FILE}")"
    signature_hex="$(sign_hash "${public_key_file%.pub}.key" "${hash_hex}")"
    signatures="$(jq --argjson signature "$(sig_json "${signature_hex}" "${fingerprint}")" '. + [$signature]' <<<"${signatures}")"
  done
  printf '%s' "${signatures}"
}

# submit_genesis_create <label> <template-id> <create-arguments-json>: one prepared transaction with one
# CreateCommand, actAs Operator and guardianGovernance. The prepare endpoint accepts one command only.
# Prints "RESULT <label> committed" or "RESULT <label> rejected". Returns non-zero unless committed.
submit_genesis_create() {
  local label="$1" template_id="$2" create_arguments="$3"
  local command_id="guardian-governance-genesis-${MODE}-${label}-$$"
  local prepare_request prepare_response prepared_transaction hash_hex hashing_scheme_version
  local operator_signature gg_signatures party_signatures execute_request http_code
  prepare_request="$(jq -n \
    --arg cmdId "${command_id}" \
    --arg op "${OP_PARTY}" \
    --arg gg "${GG_PARTY}" \
    --arg synchronizerId "${LOGICAL_SYNCHRONIZER_ID}" \
    --arg templateId "${template_id}" \
    --argjson createArguments "${create_arguments}" \
    '{
      commandId: $cmdId,
      actAs: [$op, $gg],
      synchronizerId: $synchronizerId,
      packageIdSelectionPreference: [],
      verboseHashing: false,
      hashingSchemeVersion: "HASHING_SCHEME_VERSION_V3",
      userId: "participant_admin",
      commands: [ { CreateCommand: { templateId: $templateId, createArguments: $createArguments } } ]
    }')"

  echo "[genesis:${MODE}] preparing the ${label} create command..." >&2
  if ! prepare_response="$(curl -sS --fail-with-body -X POST "${BASE_URL}/v2/interactive-submission/prepare" \
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
  gg_signatures="$(gg_signatures_json "${hash_hex}")"
  party_signatures="$(jq -n \
    --arg op "${OP_PARTY}" --argjson opSig "[${operator_signature}]" \
    --arg gg "${GG_PARTY}" --argjson ggSig "${gg_signatures}" \
    'if ($ggSig | length) > 0 then
       { signatures: [ {party: $op, signatures: $opSig}, {party: $gg, signatures: $ggSig} ] }
     else
       { signatures: [ {party: $op, signatures: $opSig} ] }
     end')"
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

  echo "[genesis:${MODE}] executing ${label} with $(jq 'length' <<<"${gg_signatures}")-of-${GG_OWNER_COUNT} guardianGovernance signature(s) (threshold ${GG_THRESHOLD_VALUE})..." >&2
  http_code="$(curl -s -o "${EXEC_RESPONSE_FILE}" -w '%{http_code}' -X POST \
    "${BASE_URL}/v2/interactive-submission/executeAndWait" -H 'Content-Type: application/json' -d "${execute_request}")"
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

genesis_status=0
submit_genesis_create CoreState "${GG_CORE_PACKAGE_ID}:Wormhole.Core.State:CoreState" "${CORE_STATE_ARGS}" \
  || genesis_status=1
submit_genesis_create EmitterRegistry "${GG_CORE_PACKAGE_ID}:Wormhole.Core.State:EmitterRegistry" "${EMITTER_REGISTRY_ARGS}" \
  || genesis_status=1
exit "${genesis_status}"
