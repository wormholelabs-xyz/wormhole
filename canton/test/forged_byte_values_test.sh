#!/usr/bin/env bash
# Table-driven test: the ledger rejects forged byte values at each core boundary.
# Runs Test.ForgedByteValues:runForgedValueCases once on the IDE ledger with every case as JSON input.
# Needs dpm and jq, and the DARs of `dpm build --all`.
# Usage: canton/test/forged_byte_values_test.sh
set -uo pipefail
CANTON_TEST_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
readonly CANTON_TEST_DIR
readonly TEST_DAR="$CANTON_TEST_DIR/.daml/dist/wormhole-core-test-0.1.0.dar"
readonly DPM_BIN="${DPM_BIN:-$(command -v dpm || echo "$HOME/.dpm/bin/dpm")}"

readonly VALID_DIGEST_HEX="aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
readonly VALID_GUARDIAN_ADDRESS_HEX="befa429d57cd18b7f8a4d91a2da9ab4af05d0fbe"
readonly DEVNET_PUBLIC_KEY_HEX="04d4a4629979f0c9fa0f0bb54edf33f87c8c5a1f42c0350a30d68f7e967023e34e495a8ebf5101036d0fd66e3b0a8c7c61b65fceeaf487ab3cd1b5b7b50beb7970"
readonly PRECONDITION_FRAGMENT="precondition"

# Columns: case | ForgedValueCase constructor | byte type | forged hex | expected outcome fragment ("accepted" for a valid control)
readonly FORGED_VALUE_CASES=(
  "CoreState governance contract uppercase|CoreStateGovernanceContract|EmitterAddress|${VALID_DIGEST_HEX^^}|$PRECONDITION_FRAGMENT"
  "CoreState governance contract 31 bytes|CoreStateGovernanceContract|EmitterAddress|${VALID_DIGEST_HEX:2}|$PRECONDITION_FRAGMENT"
  "CoreState governance contract valid|CoreStateGovernanceContract|EmitterAddress|$VALID_DIGEST_HEX|accepted"
  "CoreState guardian address 0x prefix|CoreStateGuardianAddress|GuardianAddress|0x${VALID_GUARDIAN_ADDRESS_HEX:2}|$PRECONDITION_FRAGMENT"
  "CoreState guardian address 21 bytes|CoreStateGuardianAddress|GuardianAddress|${VALID_GUARDIAN_ADDRESS_HEX}00|$PRECONDITION_FRAGMENT"
  "CoreState consumed digest odd length|CoreStateConsumedGovernanceDigest|VaaDigest|${VALID_DIGEST_HEX:1}|$PRECONDITION_FRAGMENT"
  "CoreState consumed digest valid|CoreStateConsumedGovernanceDigest|VaaDigest|$VALID_DIGEST_HEX|accepted"
  "ParseAndVerifyVAA uppercase VAA|ParseAndVerifyEncodedVaa|EncodedVaa|${VALID_DIGEST_HEX^^}${VALID_DIGEST_HEX^^}|encodedVAA is not a well-formed EncodedVaa"
  "ParseAndVerifyVAA 0x VAA|ParseAndVerifyEncodedVaa|EncodedVaa|0x${VALID_DIGEST_HEX}${VALID_DIGEST_HEX}|encodedVAA is not a well-formed EncodedVaa"
  "ParseAndVerifyVAA 64-byte raw key|ParseAndVerifyGuardianPublicKey|GuardianPublicKey|${DEVNET_PUBLIC_KEY_HEX:2}|pubKeys holds a malformed GuardianPublicKey"
  "ParseAndVerifyVAA uppercase key|ParseAndVerifyGuardianPublicKey|GuardianPublicKey|${DEVNET_PUBLIC_KEY_HEX^^}|pubKeys holds a malformed GuardianPublicKey"
  "ParseAndVerifyVAA valid key|ParseAndVerifyGuardianPublicKey|GuardianPublicKey|$DEVNET_PUBLIC_KEY_HEX|accepted"
  "SubmitGovernanceVAA odd-length VAA|SubmitGovernanceEncodedVaa|EncodedVaa|${VALID_DIGEST_HEX}${VALID_DIGEST_HEX}0|encodedVAA is not a well-formed EncodedVaa"
  "VerifyAndConsumeVAA short VAA|VerifyAndConsumeEncodedVaa|EncodedVaa|00|encodedVAA is not a well-formed EncodedVaa"
  "VerifyAndConsumeVAA key with tag 02|VerifyAndConsumeGuardianPublicKey|GuardianPublicKey|02${DEVNET_PUBLIC_KEY_HEX:2}|pubKeys holds a malformed GuardianPublicKey"
  "VerifyAndConsumeVAA valid key|VerifyAndConsumeGuardianPublicKey|GuardianPublicKey|$DEVNET_PUBLIC_KEY_HEX|accepted"
  "PublishMessage uppercase payload|PublishMessagePayload|MessagePayload|AB|payload is not a well-formed MessagePayload"
  "PublishMessage odd-length payload|PublishMessagePayload|MessagePayload|abc|payload is not a well-formed MessagePayload"
  "PublishMessage 0x payload|PublishMessagePayload|MessagePayload|0x00|payload is not a well-formed MessagePayload"
  "PublishMessage non-hex payload|PublishMessagePayload|MessagePayload|zz|payload is not a well-formed MessagePayload"
  "PublishMessage empty payload|PublishMessagePayload|MessagePayload||accepted"
  "ConsumeDigest uppercase digest|ConsumeDigestArgument|VaaDigest|${VALID_DIGEST_HEX^^}|digest is not a well-formed VaaDigest"
  "ConsumeDigest 33-byte digest|ConsumeDigestArgument|VaaDigest|${VALID_DIGEST_HEX}00|digest is not a well-formed VaaDigest"
  "ReplayNode 64-nibble prefix|ReplayNodePrefix|DigestNibblePrefix|$VALID_DIGEST_HEX|$PRECONDITION_FRAGMENT"
  "ReplayNode uppercase prefix|ReplayNodePrefix|DigestNibblePrefix|A|$PRECONDITION_FRAGMENT"
  "ReplayNode valid prefix|ReplayNodePrefix|DigestNibblePrefix|a|accepted"
  "ReplayNode empty suffix|ReplayNodeConsumedSuffix|DigestNibbleSuffix||$PRECONDITION_FRAGMENT"
  "ReplayNode non-hex suffix|ReplayNodeConsumedSuffix|DigestNibbleSuffix|${VALID_DIGEST_HEX:1}g|$PRECONDITION_FRAGMENT"
  "ReplayNode valid suffix|ReplayNodeConsumedSuffix|DigestNibbleSuffix|$VALID_DIGEST_HEX|accepted"
  "Emitter uppercase emitter address|EmitterEmitterAddress|EmitterAddress|${VALID_DIGEST_HEX^^}|$PRECONDITION_FRAGMENT"
  "Emitter valid emitter address|EmitterEmitterAddress|EmitterAddress|$VALID_DIGEST_HEX|accepted"
  "FeeWithdrawalAuthorization 20-byte recipient|FeeWithdrawalRecipient|UniversalAddress|$VALID_GUARDIAN_ADDRESS_HEX|$PRECONDITION_FRAGMENT"
  "FeeWithdrawalAuthorization 0x digest|FeeWithdrawalVaaDigest|VaaDigest|0x${VALID_DIGEST_HEX:2}|$PRECONDITION_FRAGMENT"
  "FeeWithdrawalAuthorization valid digest|FeeWithdrawalVaaDigest|VaaDigest|$VALID_DIGEST_HEX|accepted"
)

if [[ ! -f "$TEST_DAR" ]]; then
  echo "FAIL: $TEST_DAR is missing; run 'dpm build --all' in canton/" >&2
  exit 1
fi

work_dir="$(mktemp -d)"
trap 'rm -rf "$work_dir"' EXIT

case_names=()
expected_fragments=()
forged_case_json_lines=()
for forged_value_case in "${FORGED_VALUE_CASES[@]}"; do
  IFS='|' read -r case_name case_constructor byte_type forged_hex expected_fragment <<<"$forged_value_case"
  case_names+=("$case_name")
  expected_fragments+=("$expected_fragment")
  forged_case_json_lines+=("$(jq -cn --arg caseTag "$case_constructor" --arg byteType "$byte_type" --arg forgedHex "$forged_hex" \
    '{tag: $caseTag, value: {tag: $byteType, value: $forgedHex}}')")
done
printf '%s\n' "${forged_case_json_lines[@]}" | jq -s . > "$work_dir/input.json"

if ! "$DPM_BIN" script --dar "$TEST_DAR" --script-name Test.ForgedByteValues:runForgedValueCases --ide-ledger \
    --input-file "$work_dir/input.json" --output-file "$work_dir/output.json" > "$work_dir/script.log" 2>&1; then
  echo "FAIL: dpm script exited non-zero:" >&2
  tail -20 "$work_dir/script.log" >&2
  exit 1
fi

outcome_count="$(jq 'length' "$work_dir/output.json")"
if [[ "$outcome_count" != "${#FORGED_VALUE_CASES[@]}" ]]; then
  echo "FAIL: $outcome_count outcomes for ${#FORGED_VALUE_CASES[@]} cases" >&2
  exit 1
fi

failure_count=0
for case_index in "${!case_names[@]}"; do
  outcome="$(jq -r --argjson caseIndex "$case_index" '.[$caseIndex]' "$work_dir/output.json")"
  expected_fragment="${expected_fragments[$case_index]}"
  if [[ "$expected_fragment" == "accepted" ]]; then
    case_passed=$([[ "$outcome" == "accepted" ]] && echo yes || echo no)
  else
    case_passed=$([[ "$outcome" == rejected:* && "$outcome" == *"$expected_fragment"* ]] && echo yes || echo no)
  fi
  if [[ "$case_passed" == yes ]]; then
    echo "ok: ${case_names[$case_index]}"
  else
    echo "FAIL: ${case_names[$case_index]}: expected '$expected_fragment', got '$(head -c 400 <<<"$outcome")'" >&2
    failure_count=$((failure_count + 1))
  fi
done

if (( failure_count > 0 )); then
  echo "$failure_count of ${#FORGED_VALUE_CASES[@]} forged value cases failed" >&2
  exit 1
fi
echo "all ${#FORGED_VALUE_CASES[@]} forged value cases passed"
