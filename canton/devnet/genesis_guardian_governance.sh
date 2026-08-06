#!/usr/bin/env bash
#
# Demonstrate that the guardianGovernance external party, bootstrapped by
# setup_guardian_governance.sh, can actually co-sign a transaction: prepare a
# `create CoreState` command (co-signed by an external `operator` party and
# guardianGovernance, mirroring Test.TestCore:setup's
# `submit (actAs operator <> actAs guardianGovernance)` but via interactive
# submission instead of Daml Script), have owners sign the prepared hash, and
# execute.
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
#                guardian-anchored CoreState).
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

# --- Build the create-CoreState command, matching mkInitialCoreState's devnet
# values (Test.TestCore.setup / TestCore.daml). ---
COMMAND_ID="guardian-governance-genesis-${MODE}-$$"
CREATE_ARGS="$(jq -n \
  --arg operator "${OP_PARTY}" \
  --arg gg "${GG_PARTY}" \
  '{
    operator: $operator,
    guardianGovernance: $gg,
    chainId: "75",
    governanceChainId: "1",
    governanceContract: "0000000000000000000000000000000000000000000000000000000000000004",
    guardianSetIndex: "0",
    guardianSets: [["0", {"keys": ["befa429d57cd18b7f8a4d91a2da9ab4af05d0fbe"]}]],
    messageFee: "0",
    consumedGovernance: {"map": []}
  }')"

PREPARE_REQUEST="$(jq -n \
  --arg cmdId "${COMMAND_ID}" \
  --arg op "${OP_PARTY}" \
  --arg gg "${GG_PARTY}" \
  --arg synchronizerId "${LOGICAL_SYNCHRONIZER_ID}" \
  --arg templateId "${GG_CORE_PACKAGE_ID}:Wormhole.Core.State:CoreState" \
  --argjson createArgs "${CREATE_ARGS}" \
  '{
    commandId: $cmdId,
    actAs: [$op, $gg],
    synchronizerId: $synchronizerId,
    packageIdSelectionPreference: [],
    verboseHashing: false,
    hashingSchemeVersion: "HASHING_SCHEME_VERSION_V3",
    userId: "participant_admin",
    commands: [ { CreateCommand: { templateId: $templateId, createArguments: $createArgs } } ]
  }')"

echo "[genesis:${MODE}] preparing CoreState create command..." >&2
PREPARE_RESPONSE="$(curl -sf -X POST "${BASE_URL}/v2/interactive-submission/prepare" \
  -H 'Content-Type: application/json' -d "${PREPARE_REQUEST}")"
PREPARED_TX="$(echo "${PREPARE_RESPONSE}" | jq -r '.preparedTransaction')"
HASH_B64="$(echo "${PREPARE_RESPONSE}" | jq -r '.preparedTransactionHash')"
HSV="$(echo "${PREPARE_RESPONSE}" | jq -r '.hashingSchemeVersion')"
HASH_HEX="$(b64_to_hex "${HASH_B64}")"

OP_SIG_HEX="$(sign_hash "${OP_KEYFILE}" "${HASH_HEX}")"
OP_SIG_JSON="$(sig_json "${OP_SIG_HEX}" "${OP_FINGERPRINT}")"

GG_SIGNATURES="[]"
case "${MODE}" in
  happy)
    # Sign with the first GG_THRESHOLD_VALUE owners -- exactly meets threshold.
    for i in $(seq 0 $((GG_THRESHOLD_VALUE - 1))); do
      fp="$(jq -r ".owners[$i].namespace" "${GG_ARTIFACTS_FILE}")"
      pubfile="$(jq -r ".owners[$i].publicKeyFile" "${GG_ARTIFACTS_FILE}")"
      keyfile="${pubfile%.pub}.key"
      sig_hex="$(sign_hash "${keyfile}" "${HASH_HEX}")"
      GG_SIGNATURES="$(echo "${GG_SIGNATURES}" | jq --argjson s "$(sig_json "${sig_hex}" "${fp}")" '. + [$s]')"
    done
    ;;
  threshold)
    # One signature only -- below threshold, must be rejected.
    fp="$(jq -r '.owners[0].namespace' "${GG_ARTIFACTS_FILE}")"
    pubfile="$(jq -r '.owners[0].publicKeyFile' "${GG_ARTIFACTS_FILE}")"
    keyfile="${pubfile%.pub}.key"
    sig_hex="$(sign_hash "${keyfile}" "${HASH_HEX}")"
    GG_SIGNATURES="[$(sig_json "${sig_hex}" "${fp}")]"
    ;;
  forge)
    # No guardianGovernance signature at all -- operator alone must not be
    # able to forge the CoreState.
    GG_SIGNATURES="[]"
    ;;
  *)
    echo "unknown mode: ${MODE}" >&2
    exit 2
    ;;
esac

PARTY_SIGNATURES="$(jq -n \
  --arg op "${OP_PARTY}" --argjson opSig "[${OP_SIG_JSON}]" \
  --arg gg "${GG_PARTY}" --argjson ggSig "${GG_SIGNATURES}" \
  'if ($ggSig | length) > 0 then
     { signatures: [ {party: $op, signatures: $opSig}, {party: $gg, signatures: $ggSig} ] }
   else
     { signatures: [ {party: $op, signatures: $opSig} ] }
   end')"

EXECUTE_REQUEST="$(jq -n \
  --arg prepared "${PREPARED_TX}" \
  --argjson partySignatures "${PARTY_SIGNATURES}" \
  --arg submissionId "${COMMAND_ID}-exec" \
  --arg hsv "${HSV}" \
  '{
    preparedTransaction: $prepared,
    partySignatures: $partySignatures,
    submissionId: $submissionId,
    userId: "participant_admin",
    hashingSchemeVersion: $hsv,
    deduplicationPeriod: { Empty: {} }
  }')"

OWNER_COUNT="$(jq '.owners | length' "${GG_ARTIFACTS_FILE}")"
echo "[genesis:${MODE}] executing with $(echo "${GG_SIGNATURES}" | jq 'length')-of-${OWNER_COUNT} guardianGovernance signature(s) (threshold ${GG_THRESHOLD_VALUE})..." >&2
EXEC_RESPONSE_FILE="$(mktemp)"
trap 'rm -f "${EXEC_RESPONSE_FILE}"' EXIT
HTTP_CODE="$(curl -s -o "${EXEC_RESPONSE_FILE}" -w '%{http_code}' -X POST \
  "${BASE_URL}/v2/interactive-submission/execute" -H 'Content-Type: application/json' -d "${EXECUTE_REQUEST}")"

echo "[genesis:${MODE}] HTTP ${HTTP_CODE}" >&2
cat "${EXEC_RESPONSE_FILE}"
echo

if [ "${HTTP_CODE}" = "200" ]; then
  exit 0
else
  exit 1
fi
