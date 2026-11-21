#!/usr/bin/env bash
# Table test of gg_offline_lib.sh: the genesis offer and accept arguments, and the governance relay command.
# Needs bash and jq.
# Usage: canton/localnet/test/gg_offline_lib_test.sh
set -uo pipefail
LOCALNET_TEST_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
readonly LOCALNET_TEST_DIR
# shellcheck source=SCRIPTDIR/../gg_offline_lib.sh
source "$LOCALNET_TEST_DIR/../gg_offline_lib.sh"

failure_count=0
case_count=0
work_dir="$(mktemp -d)"
trap 'rm -rf "$work_dir"' EXIT

# check_case <case-name> <expected-status> <expected-stdout or "-"> <stderr-fragment or "-"> <command...>
check_case() {
  local case_name="$1" expected_status="$2" expected_stdout="$3" stderr_fragment="$4" actual_status=0 actual_stdout
  shift 4
  case_count=$((case_count + 1))
  actual_stdout="$("$@" 2> "$work_dir/stderr")" || actual_status=$?
  if [[ "$actual_status" != "$expected_status" ]]; then
    echo "FAIL: $case_name: status $actual_status, expected $expected_status ($(head -c 300 "$work_dir/stderr"))" >&2
    failure_count=$((failure_count + 1))
    return
  fi
  if [[ "$expected_stdout" != "-" && "$actual_stdout" != "$expected_stdout" ]]; then
    echo "FAIL: $case_name: output '$actual_stdout', expected '$expected_stdout'" >&2
    failure_count=$((failure_count + 1))
    return
  fi
  if [[ "$stderr_fragment" != "-" ]] && ! grep -qF -- "$stderr_fragment" "$work_dir/stderr"; then
    echo "FAIL: $case_name: stderr '$(head -c 300 "$work_dir/stderr")' lacks '$stderr_fragment'" >&2
    failure_count=$((failure_count + 1))
    return
  fi
  echo "ok: $case_name"
}

# party_id <hint> <two hex characters>: <hint>::1220<the two characters 32 times>.
party_id() {
  echo "$1::1220$(printf "$2%.0s" $(seq 1 32))"
}
GG="$(party_id guardianGovernance aa)"
FEE_CUSTODY="$(party_id feeCustody aa)"
NTT_CUSTODY="$(party_id nttCustody aa)"
OPERATOR="$(party_id LabsOperator bb)"
DSO="$(party_id DSO cc)"
RELAYER="$(party_id GovernanceRelayer dd)"
readonly GG FEE_CUSTODY NTT_CUSTODY OPERATOR DSO RELAYER
readonly GUARDIAN_ADDRESS="befa429d57cd18b7f8a4d91a2da9ab4af05d0fbe"
readonly SECOND_GUARDIAN_ADDRESS="88d7d8b32a9105d228100e72dffe2fae0705d31c"
readonly GUARDIAN_PUBLIC_KEY="04d4a4629979f0c9fa0f0bb54edf33f87c8c5a1f42c0350a30d68f7e967023e34e495a8ebf5101036d0fd66e3b0a8c7c61b65fceeaf487ab3cd1b5b7b50beb7970"
readonly SECOND_GUARDIAN_PUBLIC_KEY="04de9544a079988df10b0c847a401862b62d8074b02b994ed5a4f96e6078ee048bd5fd0b55e75345d60511e8d822f8b618c5e8e1fa7a9db841014a8a091f7c4119"
readonly GUARDIAN="$GUARDIAN_ADDRESS:$GUARDIAN_PUBLIC_KEY"
readonly SECOND_GUARDIAN="$SECOND_GUARDIAN_ADDRESS:$SECOND_GUARDIAN_PUBLIC_KEY"

# Columns: case | gg | feeCustody | nttCustody | onboarding fee admin | instrument admin | instrument id | index |
# guardians (address:publicKey, comma-separated) | expected status | stderr fragment ("-" for none)
readonly FIRST_GENESIS_PARAMETER_CASES=(
  "valid parameters|$GG|$FEE_CUSTODY|$NTT_CUSTODY|$OPERATOR|$DSO|Amulet|0|$GUARDIAN|0|-"
  "two guardians at index 4294967295|$GG|$FEE_CUSTODY|$NTT_CUSTODY|$OPERATOR|$DSO|Amulet|4294967295|$GUARDIAN,$SECOND_GUARDIAN|0|-"
  "empty gg|||$NTT_CUSTODY|$OPERATOR|$DSO|Amulet|0|$GUARDIAN|1|guardianGovernance is not"
  "gg without namespace|guardianGovernance|$FEE_CUSTODY|$NTT_CUSTODY|$OPERATOR|$DSO|Amulet|0|$GUARDIAN|1|guardianGovernance is not"
  "uppercase namespace|$(party_id guardianGovernance AA)|$FEE_CUSTODY|$NTT_CUSTODY|$OPERATOR|$DSO|Amulet|0|$GUARDIAN|1|guardianGovernance is not"
  "feeCustody is gg|$GG|$GG|$NTT_CUSTODY|$OPERATOR|$DSO|Amulet|0|$GUARDIAN|1|feeCustody is guardianGovernance"
  "nttCustody is gg|$GG|$FEE_CUSTODY|$GG|$OPERATOR|$DSO|Amulet|0|$GUARDIAN|1|nttCustody is guardianGovernance"
  "feeCustody is nttCustody|$GG|$FEE_CUSTODY|$FEE_CUSTODY|$OPERATOR|$DSO|Amulet|0|$GUARDIAN|1|feeCustody is nttCustody"
  "instrument admin is gg|$GG|$FEE_CUSTODY|$NTT_CUSTODY|$OPERATOR|$GG|Amulet|0|$GUARDIAN|1|fee instrument admin is guardianGovernance"
  "instrument admin is feeCustody|$GG|$FEE_CUSTODY|$NTT_CUSTODY|$OPERATOR|$FEE_CUSTODY|Amulet|0|$GUARDIAN|1|fee instrument admin is feeCustody"
  "empty instrument id|$GG|$FEE_CUSTODY|$NTT_CUSTODY|$OPERATOR|$DSO||0|$GUARDIAN|1|fee instrument id"
  "index above uint32|$GG|$FEE_CUSTODY|$NTT_CUSTODY|$OPERATOR|$DSO|Amulet|4294967296|$GUARDIAN|1|guardian set index is not"
  "negative index|$GG|$FEE_CUSTODY|$NTT_CUSTODY|$OPERATOR|$DSO|Amulet|-1|$GUARDIAN|1|guardian set index is not"
  "index with leading zero|$GG|$FEE_CUSTODY|$NTT_CUSTODY|$OPERATOR|$DSO|Amulet|01|$GUARDIAN|1|guardian set index is not"
  "no guardian|$GG|$FEE_CUSTODY|$NTT_CUSTODY|$OPERATOR|$DSO|Amulet|0||1|no guardian"
  "guardian address of 19 bytes|$GG|$FEE_CUSTODY|$NTT_CUSTODY|$OPERATOR|$DSO|Amulet|0|${GUARDIAN_ADDRESS:2}:$GUARDIAN_PUBLIC_KEY|1|not 40 lowercase hex"
  "uppercase guardian address|$GG|$FEE_CUSTODY|$NTT_CUSTODY|$OPERATOR|$DSO|Amulet|0|BEFA429D57CD18B7F8A4D91A2DA9AB4AF05D0FBE:$GUARDIAN_PUBLIC_KEY|1|not 40 lowercase hex"
  "zero guardian address|$GG|$FEE_CUSTODY|$NTT_CUSTODY|$OPERATOR|$DSO|Amulet|0|0000000000000000000000000000000000000000:$GUARDIAN_PUBLIC_KEY|1|zero address"
  "duplicate guardian address|$GG|$FEE_CUSTODY|$NTT_CUSTODY|$OPERATOR|$DSO|Amulet|0|$GUARDIAN,$GUARDIAN_ADDRESS:$SECOND_GUARDIAN_PUBLIC_KEY|1|duplicate guardian address"
  "guardian without a public key|$GG|$FEE_CUSTODY|$NTT_CUSTODY|$OPERATOR|$DSO|Amulet|0|$GUARDIAN_ADDRESS|1|has no public key"
  "second guardian without a public key|$GG|$FEE_CUSTODY|$NTT_CUSTODY|$OPERATOR|$DSO|Amulet|0|$GUARDIAN,$SECOND_GUARDIAN_ADDRESS|1|has no public key"
  "guardian with an empty public key|$GG|$FEE_CUSTODY|$NTT_CUSTODY|$OPERATOR|$DSO|Amulet|0|$GUARDIAN_ADDRESS:|1|public key is not"
  "public key of 64 bytes|$GG|$FEE_CUSTODY|$NTT_CUSTODY|$OPERATOR|$DSO|Amulet|0|$GUARDIAN_ADDRESS:${GUARDIAN_PUBLIC_KEY:2}|1|public key is not"
  "compressed public key tag 02|$GG|$FEE_CUSTODY|$NTT_CUSTODY|$OPERATOR|$DSO|Amulet|0|$GUARDIAN_ADDRESS:02${GUARDIAN_PUBLIC_KEY:2}|1|public key is not"
  "uppercase public key|$GG|$FEE_CUSTODY|$NTT_CUSTODY|$OPERATOR|$DSO|Amulet|0|$GUARDIAN_ADDRESS:${GUARDIAN_PUBLIC_KEY^^}|1|public key is not"
  "duplicate public key|$GG|$FEE_CUSTODY|$NTT_CUSTODY|$OPERATOR|$DSO|Amulet|0|$GUARDIAN,$SECOND_GUARDIAN_ADDRESS:$GUARDIAN_PUBLIC_KEY|1|duplicate guardian public key"
)
for parameter_case in "${FIRST_GENESIS_PARAMETER_CASES[@]}"; do
  IFS='|' read -r case_name gg fee_custody ntt_custody fee_admin instrument_admin instrument_id guardian_set_index \
    guardians expected_status stderr_fragment <<<"$parameter_case"
  check_case "first_genesis_parameters: $case_name" "$expected_status" - "$stderr_fragment" \
    first_genesis_parameters "$gg" "$fee_custody" "$ntt_custody" "$fee_admin" "$instrument_admin" "$instrument_id" \
    "$guardian_set_index" "$guardians"
done
check_case "first_genesis_parameters: seven arguments" 2 - "usage: first_genesis_parameters" \
  first_genesis_parameters "$GG" "$FEE_CUSTODY" "$NTT_CUSTODY" "$OPERATOR" "$DSO" Amulet 0

PARAMETERS="$(first_genesis_parameters "$GG" "$FEE_CUSTODY" "$NTT_CUSTODY" "$OPERATOR" "$DSO" Amulet 0 "$GUARDIAN")"
readonly PARAMETERS
check_case "first genesis: exact parameters JSON" 0 "$(jq -cn --arg gg "$GG" --arg fee "$FEE_CUSTODY" --arg ntt "$NTT_CUSTODY" \
  --arg admin "$OPERATOR" --arg dso "$DSO" --arg address "$GUARDIAN_ADDRESS" --arg publicKey "$GUARDIAN_PUBLIC_KEY" '{
    guardianGovernance: $gg, feeCustody: $fee, nttCustody: $ntt, onboardingFeeAdmin: $admin,
    genesisGuardianSet: {guardianSetIndex: "0", guardians: [{address: {tag: "GuardianAddress", value: $address},
      publicKey: {tag: "GuardianPublicKey", value: $publicKey}}]}, governanceChainId: "1",
    governanceContract: {tag: "EmitterAddress", value: "0000000000000000000000000000000000000000000000000000000000000004"},
    initialMessageFee: "0", feeInstrument: {admin: $dso, id: "Amulet"}, registrationFee: "0", replayRootClaimFee: "0",
    firstEmitterId: "0", firstNttManagerId: "0",
    coreGovernanceReplayRecord: {consumedGovernance: [], evictedGovernanceMaxTimestampSeconds: null},
    nttGovernanceReplayRecord: {consumedGovernance: [], evictedGovernanceMaxTimestampSeconds: null},
    genesisKind: {tag: "FirstGenesis", value: {}}}')" - echo "$PARAMETERS"
check_case "first genesis: no 0.1.0 consumed digest list" 0 false - \
  jq -c 'has("consumedCoreGovernanceDigests") or has("consumedNttGovernanceDigests")' <<<"$PARAMETERS"

OFFER="$(genesis_offer_arguments "$OPERATOR" "$PARAMETERS")"
readonly OFFER
check_case "genesis_offer_arguments: operator and parameters" 0 \
  "$(jq -cn --arg operator "$OPERATOR" --argjson parameters "$PARAMETERS" '{operator: $operator, parameters: $parameters}')" - \
  genesis_offer_arguments "$OPERATOR" "$PARAMETERS"
check_case "genesis_offer_arguments: empty operator" 1 - "operator is not" genesis_offer_arguments "" "$PARAMETERS"
check_case "genesis_offer_arguments: operator is gg" 1 - "operator is guardianGovernance" genesis_offer_arguments "$GG" "$PARAMETERS"
check_case "genesis_offer_arguments: operator is feeCustody" 1 - "operator is feeCustody" \
  genesis_offer_arguments "$FEE_CUSTODY" "$PARAMETERS"
check_case "genesis_offer_arguments: operator is nttCustody" 1 - "operator is nttCustody" \
  genesis_offer_arguments "$NTT_CUSTODY" "$PARAMETERS"
check_case "genesis_offer_arguments: parameters without nttCustody" 1 - "genesis parameters have no nttCustody" \
  genesis_offer_arguments "$OPERATOR" "$(jq -c 'del(.nttCustody)' <<<"$PARAMETERS")"
check_case "genesis_offer_arguments: parameters not JSON" 1 - "genesis parameters have no guardianGovernance" \
  genesis_offer_arguments "$OPERATOR" "not json"

# Columns: case | offer create argument JSON | agreed parameters JSON | expected status | stderr fragment
readonly ACCEPT_GENESIS_CASES=(
  "offer equals the agreed parameters|$OFFER|$PARAMETERS|0|-"
  "offer keys in another order|$(jq -c '{parameters, operator}' <<<"$OFFER")|$(jq -cS . <<<"$PARAMETERS")|0|-"
  "mismatched guardian address|$OFFER|$(jq -c --arg address "$SECOND_GUARDIAN_ADDRESS" '.genesisGuardianSet.guardians[0].address.value = $address' <<<"$PARAMETERS")|1|genesis offer parameters differ from the agreed parameters"
  "guardian address as plain text|$OFFER|$(jq -c '.genesisGuardianSet.guardians[0].address |= .value' <<<"$PARAMETERS")|1|genesis offer parameters differ from the agreed parameters"
  "mismatched guardian set index|$OFFER|$(jq -c '.genesisGuardianSet.guardianSetIndex = "1"' <<<"$PARAMETERS")|1|genesis offer parameters differ from the agreed parameters"
  "mismatched onboarding fee admin|$OFFER|$(jq -c --arg admin "$RELAYER" '.onboardingFeeAdmin = $admin' <<<"$PARAMETERS")|1|genesis offer parameters differ from the agreed parameters"
  "mismatched first emitter id|$OFFER|$(jq -c '.firstEmitterId = "7"' <<<"$PARAMETERS")|1|genesis offer parameters differ from the agreed parameters"
  "mismatched core replay record digests|$OFFER|$(jq -c '.coreGovernanceReplayRecord.consumedGovernance = [[{tag: "VaaDigest", value: "'"$(printf 'ab%.0s' $(seq 1 32))"'"}, "1"]]' <<<"$PARAMETERS")|1|genesis offer parameters differ from the agreed parameters"
  "mismatched NTT replay record floor|$OFFER|$(jq -c '.nttGovernanceReplayRecord.evictedGovernanceMaxTimestampSeconds = "1"' <<<"$PARAMETERS")|1|genesis offer parameters differ from the agreed parameters"
  "mismatched genesis kind|$OFFER|$(jq -c '.genesisKind.tag = "ReGenesisReplacing"' <<<"$PARAMETERS")|1|genesis offer parameters differ from the agreed parameters"
  "agreed parameters with an extra key|$OFFER|$(jq -c '. + {operator: "x"}' <<<"$PARAMETERS")|1|genesis offer parameters differ from the agreed parameters"
  "offer with an extra key|$(jq -c '. + {observers: []}' <<<"$OFFER")|$PARAMETERS|1|genesis offer keys are not exactly operator and parameters"
  "offer without parameters|$(jq -c 'del(.parameters)' <<<"$OFFER")|$PARAMETERS|1|genesis offer keys are not exactly operator and parameters"
  "offer not JSON|not json|$PARAMETERS|1|genesis offer keys are not exactly operator and parameters"
  "agreed parameters empty|$OFFER|{}|1|agreed genesis parameters are not a JSON object"
  "agreed parameters not JSON|$OFFER|not json|1|agreed genesis parameters are not a JSON object"
)
for accept_case in "${ACCEPT_GENESIS_CASES[@]}"; do
  IFS='|' read -r case_name offer agreed_parameters expected_status stderr_fragment <<<"$accept_case"
  expected_stdout="-"
  if [[ "$expected_status" == 0 ]]; then
    expected_stdout="$(jq -cn --argjson agreed "$agreed_parameters" '{agreedParameters: $agreed}')"
  fi
  check_case "accept_genesis_arguments: $case_name" "$expected_status" "$expected_stdout" "$stderr_fragment" \
    accept_genesis_arguments "$offer" "$agreed_parameters"
done

readonly CORE_STATE_CID="00c0ffee"
readonly VAA_HEX="010000000001"
readonly PUBLIC_KEY_HEX="04d4a4629979f0c9fa0f0bb54edf33f87c8c5a1f42c0350a30d68f7e967023e34e495a8ebf5101036d0fd66e3b0a8c7c61b65fceeaf487ab3cd1b5b7b50beb7970"
# Columns: case | CoreState cid | submitter | VAA hex | expected status | stderr fragment
readonly RELAY_GOVERNANCE_CASES=(
  "valid relay|$CORE_STATE_CID|$RELAYER|$VAA_HEX|0|-"
  "empty VAA|$CORE_STATE_CID|$RELAYER||1|governance VAA is empty"
  "VAA with an odd hex length|$CORE_STATE_CID|$RELAYER|01000|1|governance VAA is not lowercase hex"
  "uppercase VAA|$CORE_STATE_CID|$RELAYER|01AB|1|governance VAA is not lowercase hex"
  "VAA with a 0x prefix|$CORE_STATE_CID|$RELAYER|0x0100|1|governance VAA is not lowercase hex"
  "empty CoreState cid||$RELAYER|$VAA_HEX|1|CoreState contract id is not"
  "empty submitter|$CORE_STATE_CID||$VAA_HEX|1|submitter is not"
)
for relay_case in "${RELAY_GOVERNANCE_CASES[@]}"; do
  IFS='|' read -r case_name core_state_cid submitter vaa_hex expected_status stderr_fragment <<<"$relay_case"
  expected_stdout="-"
  if [[ "$expected_status" == 0 ]]; then
    expected_stdout="$(jq -cn --arg cid "$core_state_cid" --arg submitter "$submitter" --arg vaa "$vaa_hex" \
      '{ExerciseCommand: {templateId: "#wormhole-core:Wormhole.Core.State:CoreState", contractId: $cid, choice: "SubmitGovernanceVAA",
        choiceArgument: {submitter: $submitter, encodedVAA: {tag: "EncodedVaa", value: $vaa}}}}')"
  fi
  check_case "relay_governance_vaa_command: $case_name" "$expected_status" "$expected_stdout" "$stderr_fragment" \
    relay_governance_vaa_command "$core_state_cid" "$submitter" "$vaa_hex"
done

# Columns: case | CoreState cid | submitter | guardian set index | public keys JSON array | expected status | stderr fragment
readonly ATTACH_GUARDIAN_PUBLIC_KEYS_CASES=(
  "one key|$CORE_STATE_CID|$RELAYER|1|[\"$PUBLIC_KEY_HEX\"]|0|-"
  "two keys at index 4294967295|$CORE_STATE_CID|$RELAYER|4294967295|[\"$PUBLIC_KEY_HEX\",\"$SECOND_GUARDIAN_PUBLIC_KEY\"]|0|-"
  "no key|$CORE_STATE_CID|$RELAYER|1|[]|1|public keys are not"
  "keys not JSON|$CORE_STATE_CID|$RELAYER|1|not json|1|public keys are not"
  "key of 64 bytes|$CORE_STATE_CID|$RELAYER|1|[\"${PUBLIC_KEY_HEX:2}\"]|1|public keys are not"
  "uppercase key|$CORE_STATE_CID|$RELAYER|1|[\"${PUBLIC_KEY_HEX^^}\"]|1|public keys are not"
  "key in the GuardianPublicKey variant shape|$CORE_STATE_CID|$RELAYER|1|[{\"tag\":\"GuardianPublicKey\",\"value\":\"$PUBLIC_KEY_HEX\"}]|1|public keys are not"
  "duplicate key|$CORE_STATE_CID|$RELAYER|1|[\"$PUBLIC_KEY_HEX\",\"$PUBLIC_KEY_HEX\"]|1|duplicate public key"
  "index above uint32|$CORE_STATE_CID|$RELAYER|4294967296|[\"$PUBLIC_KEY_HEX\"]|1|guardian set index is not"
  "index with leading zero|$CORE_STATE_CID|$RELAYER|01|[\"$PUBLIC_KEY_HEX\"]|1|guardian set index is not"
  "empty CoreState cid||$RELAYER|1|[\"$PUBLIC_KEY_HEX\"]|1|CoreState contract id is not"
  "empty submitter|$CORE_STATE_CID||1|[\"$PUBLIC_KEY_HEX\"]|1|submitter is not"
)
for attach_case in "${ATTACH_GUARDIAN_PUBLIC_KEYS_CASES[@]}"; do
  IFS='|' read -r case_name core_state_cid submitter guardian_set_index public_keys expected_status stderr_fragment <<<"$attach_case"
  expected_stdout="-"
  if [[ "$expected_status" == 0 ]]; then
    expected_stdout="$(jq -cn --arg cid "$core_state_cid" --arg submitter "$submitter" --arg index "$guardian_set_index" \
      --argjson keys "$public_keys" \
      '{ExerciseCommand: {templateId: "#wormhole-core:Wormhole.Core.State:CoreState", contractId: $cid, choice: "AttachGuardianPublicKeys",
        choiceArgument: {submitter: $submitter, targetGuardianSetIndex: $index,
          guardianPublicKeys: [$keys[] | {tag: "GuardianPublicKey", value: .}]}}}')"
  fi
  check_case "attach_guardian_public_keys_command: $case_name" "$expected_status" "$expected_stdout" "$stderr_fragment" \
    attach_guardian_public_keys_command "$core_state_cid" "$submitter" "$guardian_set_index" "$public_keys"
done

DISCLOSURE="$(jq -cn --arg cid "$CORE_STATE_CID" '{templateId: "aa11:Wormhole.Core.State:CoreState", contractId: $cid,
  createdEventBlob: "CgMyLjES", synchronizerId: "global-domain::1220aa"}')"
readonly DISCLOSURE
# Columns: case | disclosure JSON | expected status | stderr fragment
readonly CORE_STATE_DISCLOSURE_CASES=(
  "disclosure of the CoreState|$DISCLOSURE|0|-"
  "missing disclosure||1|CoreState disclosure is not"
  "disclosure null|null|1|CoreState disclosure is not"
  "disclosure of another contract|$(jq -c '.contractId = "00beef"' <<<"$DISCLOSURE")|1|CoreState disclosure is for 00beef"
  "disclosure of another template|$(jq -c '.templateId = "aa11:Wormhole.Core.State:EmitterRegistry"' <<<"$DISCLOSURE")|1|expected Wormhole.Core.State:CoreState"
  "disclosure with an extra key|$(jq -c '. + {witnessParties: []}' <<<"$DISCLOSURE")|1|CoreState disclosure is not"
  "disclosure without a blob|$(jq -c 'del(.createdEventBlob)' <<<"$DISCLOSURE")|1|CoreState disclosure is not"
  "disclosure with an empty blob|$(jq -c '.createdEventBlob = ""' <<<"$DISCLOSURE")|1|empty created event blob"
)
for disclosure_case in "${CORE_STATE_DISCLOSURE_CASES[@]}"; do
  IFS='|' read -r case_name disclosure expected_status stderr_fragment <<<"$disclosure_case"
  expected_stdout="-"
  if [[ "$expected_status" == 0 ]]; then
    expected_stdout="[$disclosure]"
  fi
  check_case "core_state_disclosures: $case_name" "$expected_status" "$expected_stdout" "$stderr_fragment" \
    core_state_disclosures "$disclosure" "$CORE_STATE_CID"
done

if [[ "$failure_count" -ne 0 ]]; then
  echo "gg_offline_lib_test: $failure_count of $case_count cases failed" >&2
  exit 1
fi
echo "gg_offline_lib_test: $case_count cases passed"
