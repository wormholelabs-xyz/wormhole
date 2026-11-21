#!/usr/bin/env bash
# Table-driven test of ntt_offline_lib.sh. Needs bash, jq, zip and unzip.
# Usage: canton/localnet/test/ntt_lib_test.sh
set -uo pipefail
LOCALNET_TEST_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
readonly LOCALNET_TEST_DIR
# shellcheck source=SCRIPTDIR/../env.sh
source "$LOCALNET_TEST_DIR/../env.sh"
# shellcheck source=SCRIPTDIR/../ntt_offline_lib.sh
source "$LOCALNET_TEST_DIR/../ntt_offline_lib.sh"

failure_count=0
work_dir="$(mktemp -d)"
trap 'rm -rf "$work_dir"' EXIT

# check_case <case-name> <expected-status> <expected-stdout or "-"> <stderr-fragment or "-"> <command...>
check_case() {
  local case_name="$1" expected_status="$2" expected_stdout="$3" stderr_fragment="$4" actual_status=0 actual_stdout
  shift 4
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

readonly CREATED_COIN_TRANSACTION='{"events": [
  {"CreatedEvent": {"templateId": "pkg:Token.CIP0056.Coin:Coin", "createArgument": {"amount": "2.5"}}},
  {"ExercisedEvent": {"templateId": "pkg:Wormhole.Ntt.Manager:NttManager", "choice": "Mint", "exerciseResult": {"amount": "250000000"}}}]}'

check_case "json_array of two values" 0 '[{"a":1},2]' - json_array '{"a": 1}' 2
check_case "created_contract_argument of one created event" 0 '{"amount":"2.5"}' - \
  created_contract_argument "$CREATED_COIN_TRANSACTION" ":Token.CIP0056.Coin:Coin"
check_case "created_contract_argument with no match" 5 - "found 0" \
  created_contract_argument "$CREATED_COIN_TRANSACTION" ":Wormhole.Ntt.Ledger:LockedLedger"

readonly MANAGER_TRANSFER_EVENT='{"ExercisedEvent": {"templateId": "pkg:Wormhole.Ntt.Manager:NttManager", "choice": "Transfer", "exerciseResult": {"sequence": "2"}}}'
readonly FACTORY_TRANSFER_EVENT='{"ExercisedEvent": {"templateId": "pkg:Splice.Api.Token.TransferInstructionV1:TransferFactory", "choice": "Transfer", "exerciseResult": {"output": "completed"}}}'
check_case "exercise_result with one match" 0 '{"sequence":"2"}' - \
  exercise_result "{\"events\": [$MANAGER_TRANSFER_EVENT]}" ":Wormhole.Ntt.Manager:NttManager" Transfer
check_case "exercise_result skips the same choice on another template" 0 '{"sequence":"2"}' - \
  exercise_result "{\"events\": [$FACTORY_TRANSFER_EVENT, $MANAGER_TRANSFER_EVENT]}" ":Wormhole.Ntt.Manager:NttManager" Transfer
check_case "exercise_result with two matches" 5 - "found 2" \
  exercise_result "{\"events\": [$MANAGER_TRANSFER_EVENT, $MANAGER_TRANSFER_EVENT]}" ":Wormhole.Ntt.Manager:NttManager" Transfer
check_case "exercise_result with no match" 5 - "found 0" \
  exercise_result "{\"events\": [$FACTORY_TRANSFER_EVENT]}" ":Wormhole.Ntt.Manager:NttManager" Transfer

# Columns: case | decimal text | decimals | expected units ("-" for an error)
readonly DECIMAL_TO_UNITS_CASES=(
  "fraction|2.5|10|25000000000"
  "integer|3|10|30000000000"
  "smallest unit|0.0000000001|10|1"
  "all fraction digits|1.5000000000|10|15000000000"
  "zero|0|10|0"
  "zero decimals|7|0|7"
  "largest 18-digit amount|99999999.9999999999|10|999999999999999999"
  "too many fraction digits|1.00000000001|10|-"
  "negative|-1|10|-"
  "exponent|1e3|10|-"
  "empty||10|-"
  "leading dot|.5|10|-"
  "trailing dot|5.|10|-"
  "above 18 digits|100000000|10|-"
  "decimals not a number|1|ten|-"
)
for decimal_case in "${DECIMAL_TO_UNITS_CASES[@]}"; do
  IFS='|' read -r case_name decimal_text decimals expected_units <<<"$decimal_case"
  if [[ "$expected_units" == "-" ]]; then
    check_case "decimal_to_units $case_name" 1 - - decimal_to_units "$decimal_text" "$decimals"
  else
    check_case "decimal_to_units $case_name" 0 "$expected_units" - decimal_to_units "$decimal_text" "$decimals"
  fi
done
check_case "decimal_texts_total_units 0.1 + 0.2" 0 3000000000 - decimal_texts_total_units 10 0.1 0.2
check_case "decimal_texts_total_units of no amounts" 0 0 - decimal_texts_total_units 10
check_case "decimal_texts_total_units with a bad amount" 1 - - decimal_texts_total_units 10 0.1 1e3
check_case "decimal_texts_total_units above 18 digits" 1 - "18 digits" \
  decimal_texts_total_units 10 99999999.9999999999 0.0000000001

check_case "json_decimal_array_total_units of two amounts" 0 25000000000 - \
  json_decimal_array_total_units 10 '["1.0000000000", "1.5"]'
check_case "json_decimal_array_total_units of an empty array" 1 - "non-empty" json_decimal_array_total_units 10 '[]'
check_case "json_decimal_array_total_units of a number" 1 - "non-empty" json_decimal_array_total_units 10 '[2.5]'
check_case "json_decimal_array_total_units with a space in an amount" 1 - - json_decimal_array_total_units 10 '["1 2"]'

# file_mode_octal <file>: the permission bits, for example 600.
file_mode_octal() {
  # GNU form first: GNU stat reads -f as file-system mode and prints to stdout.
  if stat -c %a "$1" 2>/dev/null; then return 0; fi
  stat -f %Lp "$1"
}

check_case "ntt scripts pass no bearer token as a command argument" 1 "" - \
  grep -nE 'Authorization: Bearer \$' "$LOCALNET_TEST_DIR"/../ntt_*.sh
mkdir -p "$work_dir/stub-localnet" "$work_dir/auth-headers"
cat > "$work_dir/stub-localnet/token.sh" <<'STUB_TOKEN_SCRIPT'
#!/usr/bin/env bash
echo "stub-token-for-$1"
STUB_TOKEN_SCRIPT
chmod +x "$work_dir/stub-localnet/token.sh"
# write_auth_header_file (env.sh) runs $LOCALNET_SCRIPT_DIR/token.sh.
LOCALNET_SCRIPT_DIR="$work_dir/stub-localnet"
gg_work_dir="$work_dir/auth-headers"
subject_header_file="$(subject_auth_header_file ntt-locker-1 2> "$work_dir/stderr")" \
  || echo "subject_auth_header_file failed: $(cat "$work_dir/stderr")" >&2
check_case "subject header file has mode 600" 0 600 - file_mode_octal "$subject_header_file"
check_case "subject header file holds the header" 0 "Authorization: Bearer stub-token-for-ntt-locker-1" - cat "$subject_header_file"
check_case "subject header file is reused" 0 "$subject_header_file" - subject_auth_header_file ntt-locker-1
check_case "subject header file of a path subject" 1 - "subject" subject_auth_header_file ../escape

# Columns: case | NTT_RELEASE_AFTER_ROUNDS text | expected status
readonly RELEASE_AFTER_ROUNDS_CASES=(
  "zero|0|0"
  "maximum|24|0"
  "above the maximum|25|1"
  "leading zero is decimal|08|0"
  "arithmetic expression|1+1|1"
  "command substitution|a[\$(id)]|1"
  "empty||1"
  "negative|-1|1"
  "four digits|9999|1"
)
for release_case in "${RELEASE_AFTER_ROUNDS_CASES[@]}"; do
  IFS='|' read -r case_name rounds_text expected_status <<<"$release_case"
  check_case "check_release_after_rounds $case_name" "$expected_status" - - check_release_after_rounds "$rounds_text"
done

# name|version|expected status|expected package id
readonly NTT_PIN_CASES=(
  "ntt 0.6.1|0.6.1|0|37753fce5b8069ada4dd56523e36fe5e0abe86be784d49854e014ec248f0c3fc"
  "replaced 0.6.0 on core 0.12.0|0.6.0|1|-"
  "replaced 0.5.0 on core 0.9.0|0.5.0|1|-"
  "retired 0.3.4 on core 0.8.3|0.3.4|1|-"
  "retired 0.4.2 on core 0.8.3|0.4.2|1|-"
  "retired 0.3.2 on core 0.8.0|0.3.2|1|-"
  "retired 0.4.0 on core 0.8.0|0.4.0|1|-"
  "unknown version|0.4.1|1|-"
  "empty version||1|-"
  "version with suffix|0.6.1-rc1|1|-"
)
for pin_case in "${NTT_PIN_CASES[@]}"; do
  IFS='|' read -r case_name pin_version expected_status expected_package_id <<<"$pin_case"
  check_case "ntt_pinned_package_id $case_name" "$expected_status" "$expected_package_id" - ntt_pinned_package_id "$pin_version"
done
check_case "ntt_pinned_package_id names the pinned versions" 1 - "pinned versions: 0.6.1" ntt_pinned_package_id 9.9.9
check_case "ntt_requires_custody_party 0.6.1" 0 - - ntt_requires_custody_party 0.6.1
check_case "ntt_requires_custody_party 0.5.0" 1 - - ntt_requires_custody_party 0.5.0
check_case "ntt_requires_custody_party 0.3.4" 1 - - ntt_requires_custody_party 0.3.4

readonly HEX_32_BYTES="000000000000000000000000e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0"
# Columns: case | type name | hex | expected status | expected stdout ("-" for an error) | stderr fragment
readonly BYTE_VARIANT_JSON_CASES=(
  "NttManagerAddress|NttManagerAddress|$HEX_32_BYTES|0|{\"tag\":\"NttManagerAddress\",\"value\":\"$HEX_32_BYTES\"}|-"
  "one byte|EncodedVaa|01|0|{\"tag\":\"EncodedVaa\",\"value\":\"01\"}|-"
  "empty hex|EncodedVaa||1|-|not lowercase hex of whole bytes"
  "odd hex length|EncodedVaa|010|1|-|not lowercase hex of whole bytes"
  "uppercase hex|UniversalAddress|${HEX_32_BYTES^^}|1|-|not lowercase hex of whole bytes"
  "0x prefix|UniversalAddress|0x$HEX_32_BYTES|1|-|not lowercase hex of whole bytes"
  "type name with a space|Ntt Manager|01|1|-|type name"
  "empty type name||01|1|-|type name"
)
for byte_variant_case in "${BYTE_VARIANT_JSON_CASES[@]}"; do
  IFS='|' read -r case_name type_name hex_text expected_status expected_stdout stderr_fragment <<<"$byte_variant_case"
  check_case "byte_variant_json $case_name" "$expected_status" "$expected_stdout" "$stderr_fragment" \
    byte_variant_json "$type_name" "$hex_text"
done
# Columns: case | type name | variant JSON | expected status | expected stdout ("-" for an error) | stderr fragment
readonly BYTE_VARIANT_HEX_CASES=(
  "EmitterAddress|EmitterAddress|{\"tag\":\"EmitterAddress\",\"value\":\"$HEX_32_BYTES\"}|0|$HEX_32_BYTES|-"
  "keys in another order|MessagePayload|{\"value\":\"9c23\",\"tag\":\"MessagePayload\"}|0|9c23|-"
  "another tag|NttManagerAddress|{\"tag\":\"EmitterAddress\",\"value\":\"$HEX_32_BYTES\"}|1|-|is not the variant {tag: NttManagerAddress"
  "plain hex text|EmitterAddress|\"$HEX_32_BYTES\"|1|-|is not the variant {tag: EmitterAddress"
  "extra key|EmitterAddress|{\"tag\":\"EmitterAddress\",\"value\":\"01\",\"x\":1}|1|-|is not the variant {tag: EmitterAddress"
  "uppercase value|EmitterAddress|{\"tag\":\"EmitterAddress\",\"value\":\"AB\"}|1|-|is not the variant {tag: EmitterAddress"
  "empty value|EmitterAddress|{\"tag\":\"EmitterAddress\",\"value\":\"\"}|1|-|is not the variant {tag: EmitterAddress"
  "null|EmitterAddress|null|1|-|is not the variant {tag: EmitterAddress"
  "not JSON|EmitterAddress|not json|1|-|is not the variant {tag: EmitterAddress"
)
for byte_variant_case in "${BYTE_VARIANT_HEX_CASES[@]}"; do
  IFS='|' read -r case_name type_name variant_json expected_status expected_stdout stderr_fragment <<<"$byte_variant_case"
  check_case "byte_variant_hex $case_name" "$expected_status" "$expected_stdout" "$stderr_fragment" \
    byte_variant_hex "$type_name" "$variant_json"
done
readonly WORMHOLE_MESSAGE_0_12='{"registrar": "guardianGovernance::1220aa", "owner": "guardianGovernance::1220aa", "emitterId": "3", "sequence": "0", "nonce": "0", "consistencyLevel": "0", "payload": {"tag": "MessagePayload", "value": "9c23bd3b0102"}}'
check_case "message_summary reads the MessagePayload variant" 0 \
  '{"registrar":"guardianGovernance::1220","owner":"guardianGovernance::1220","emitterId":"3","sequence":"0","nonce":"0","consistencyLevel":"0","payloadPrefix":"9c23bd3b","payloadBytes":6}' - \
  message_summary "$WORMHOLE_MESSAGE_0_12"
check_case "message_summary of a plain hex payload" 5 - - \
  message_summary '{"registrar": "r", "owner": "o", "emitterId": "3", "sequence": "0", "nonce": "0", "consistencyLevel": "0", "payload": "9c23"}'

NTT_CONTRACTS="$work_dir/contracts/ntt-contracts.json"
mkdir -p "$work_dir/contracts"
check_case "record_ntt_json without a contracts file" 1 - "does not exist" record_ntt_json burnMint '{"user": "u"}'
check_case "record_ntt_contract creates the file" 0 - - record_ntt_contract coinFactory "00cid"
check_case "record_ntt_json keeps other keys" 0 - - record_ntt_json burnMint '{"user": "u"}'
check_case "contracts file after two records" 0 '{"burnMint":{"user":"u"},"coinFactory":"00cid"}' - jq -cS . "$NTT_CONTRACTS"
check_case "record_ntt_json with invalid JSON" 1 - - record_ntt_json lockUnlock '{not json'
check_case "contracts file unchanged after invalid JSON" 0 '{"burnMint":{"user":"u"},"coinFactory":"00cid"}' - \
  jq -cS . "$NTT_CONTRACTS"
check_case "no temporary contracts file stays" 0 "ntt-contracts.json" - ls "$work_dir/contracts"

readonly REJECTED_REPLAY_RESPONSE=$'{"code": "DAML_FAILURE", "cause": "... digest already consumed ..."}\n400'
readonly REJECTED_QUORUM_RESPONSE=$'{"code": "DAML_FAILURE", "cause": "no quorum"}\n400'
readonly ACCEPTED_RESPONSE=$'{"transaction": {"updateId": "1220ab"}}\n200'
check_case "classify_submit_response with the expected rejection" 0 - - \
  classify_submit_response "$REJECTED_REPLAY_RESPONSE" "digest already consumed"
check_case "classify_submit_response with another rejection" 1 - "lacks 'digest already consumed'" \
  classify_submit_response "$REJECTED_QUORUM_RESPONSE" "digest already consumed"
check_case "classify_submit_response with an accepted command" 1 - "was accepted" \
  classify_submit_response "$ACCEPTED_RESPONSE" "digest already consumed"
check_case "classify_submit_response with an empty body" 1 - "lacks" \
  classify_submit_response $'\n500' "digest already consumed"
check_case "classify_submit_response with no status line" 1 - "HTTP status" \
  classify_submit_response "digest already consumed" "digest already consumed"
check_case "classify_submit_response with an empty expected text" 1 - "expected text" \
  classify_submit_response "$REJECTED_REPLAY_RESPONSE" ""

readonly REPOSITORY_ROOT="$LOCALNET_TEST_DIR/../../.."
readonly VENDORED_DAR_DIR="$REPOSITORY_ROOT/canton/dars"
readonly CORE_0_8_0_PACKAGE_ID="03a65dc33bc2e08ad04318e300227e3f4dc6b983be09b650b59f2676f43a22b6"
readonly CORE_0_8_1_PACKAGE_ID="4a214c8015945441a74dcc2a702f0652b3136e8f7488ab13d8196313c4d47d89"
readonly CORE_0_8_2_PACKAGE_ID="132d32bfdba695df16b78b1c2250579d9369b13fe030d951973293f4e23b70c1"

# pinned_value_in_ntt_lib <readonly-name>: the value of that pin in ntt_lib.sh, which must define it exactly once.
pinned_value_in_ntt_lib() {
  local pinned_values
  pinned_values="$(sed -nE "s/^readonly $1=\"([^\"]+)\"\$/\\1/p" "$LOCALNET_TEST_DIR/../ntt_lib.sh")"
  if [[ "$(grep -c . <<<"$pinned_values")" -ne 1 ]]; then
    echo "ntt_lib.sh defines $1 $(grep -c . <<<"$pinned_values") times, expected 1" >&2
    return 1
  fi
  echo "$pinned_values"
}
pinned_core_package_id="$(pinned_value_in_ntt_lib WORMHOLE_CORE_PACKAGE_ID)" || failure_count=$((failure_count + 1))
pinned_core_version="$(pinned_value_in_ntt_lib WORMHOLE_CORE_VERSION)" || failure_count=$((failure_count + 1))
pinned_ntt_version="$(sed -nE 's/^readonly NTT_VERSION="\$\{NTT_PIN_VERSION:-([0-9.]+)\}"$/\1/p' "$LOCALNET_TEST_DIR/../ntt_lib.sh")"
pinned_ntt_package_id="$(ntt_pinned_package_id "$pinned_ntt_version")" || failure_count=$((failure_count + 1))
pinned_genesis_package_id="$(pinned_value_in_ntt_lib WORMHOLE_GENESIS_PACKAGE_ID)" || failure_count=$((failure_count + 1))
pinned_genesis_version="$(pinned_value_in_ntt_lib WORMHOLE_GENESIS_VERSION)" || failure_count=$((failure_count + 1))
check_case "pinned core id is the SDK 3.7.3 0.12.3 build 32abbcd2" 0 - - \
  test "$pinned_core_package_id" == "32abbcd2a6b5ebc2530f8e2cfc3fd1b853ed496443c6a0125fd2cea617b33371"
check_case "pinned core id is not the replaced SDK 3.7.3 0.12.2 build e162e0c0" 0 - - \
  test "$pinned_core_package_id" != "e162e0c0d15b9e884378ee346a2d57f07e0680ac58f9c2c4bf82a86a25fb5ccd"
check_case "pinned core id is not the replaced SDK 3.7.3 0.12.1 build 2ebee1d9" 0 - - \
  test "$pinned_core_package_id" != "2ebee1d99f4bb435c025fd1d63d4c0646a081498b403a6a83f8b17646bc92ef4"
check_case "pinned core id is not the replaced SDK 3.7.3 0.12.0 build aec76d21" 0 - - \
  test "$pinned_core_package_id" != "aec76d21ac140c49f91754db4667e0572d24d3b1cacd85b69dc76c92ae38087b"
check_case "pinned core id is not the replaced SDK 3.7.3 0.11.1 build 019609a3" 0 - - \
  test "$pinned_core_package_id" != "019609a33c112e13fa98642f1d5b4dd75e8ab7bfa00a14fcf57caa510ed1b9d5"
check_case "pinned core id is not the replaced SDK 3.7.3 0.11.0 build 3e555cc2" 0 - - \
  test "$pinned_core_package_id" != "3e555cc2975764a29323906d74ab513025a3ba96376210b69daa542b294352bc"
check_case "pinned core id is not the replaced SDK 3.7.3 0.10.0 build bf75fd51" 0 - - \
  test "$pinned_core_package_id" != "bf75fd518ed8f524832ac25c65a353215bb472a531d7ef745bf050848936eddb"
check_case "pinned core id is not the replaced SDK 3.7.3 0.9.0 build a562ac78" 0 - - \
  test "$pinned_core_package_id" != "a562ac78ce2617dda9dc1ccd8c7500b4e523ff7ae1d216e28f8836b1fb264034"
check_case "pinned core id is not the replaced SDK 3.5.1 0.9.0 build bba94798" 0 - - \
  test "$pinned_core_package_id" != "bba94798a73c0fafadd836ec6754cab75205cb09556d13263813151bd748006a"
check_case "pinned core version" 0 - - test "$pinned_core_version" == "0.12.3"
check_case "pinned core id is not the replaced first 0.9.0 build dd75f777" 0 - - \
  test "$pinned_core_package_id" != "dd75f77780f2b93e5a30f12a04f54f9e4577c12ebcaf1720a413f09cc229af1d"
check_case "pinned core id is not the replaced second 0.9.0 build ec382591" 0 - - \
  test "$pinned_core_package_id" != "ec382591e253d8e5c9e8cd4f1de5fd3c40cb655c9f78da2d9de15af74958d66a"
check_case "pinned core id is not the withdrawn 0.8.3 9d546b11" 0 - - \
  test "$pinned_core_package_id" != "9d546b11014d1272be6b09a0ff9f6204a3401333829531d360e20fabd1b4163d"
check_case "pinned core id is not the withdrawn first 0.8.3 build b5613c61" 0 - - \
  test "$pinned_core_package_id" != "b5613c61c1aac18db84a87e2eb13cde6d544dd953d10c40de0440747e92a7e69"
check_case "pinned core id is not the withdrawn 0.8.0 03a65dc3" 0 - - \
  test "$pinned_core_package_id" != "$CORE_0_8_0_PACKAGE_ID"
check_case "pinned core id is not the withdrawn 0.8.1 4a214c80" 0 - - \
  test "$pinned_core_package_id" != "$CORE_0_8_1_PACKAGE_ID"
check_case "pinned core id is not the withdrawn 0.8.2 132d32bf" 0 - - \
  test "$pinned_core_package_id" != "$CORE_0_8_2_PACKAGE_ID"
check_case "pinned ntt id is 0.6.1 37753fce" 0 - - \
  test "$pinned_ntt_package_id" == "37753fce5b8069ada4dd56523e36fe5e0abe86be784d49854e014ec248f0c3fc"
check_case "pinned ntt id is not the replaced 0.6.0 79a1cc2c" 0 - - \
  test "$pinned_ntt_package_id" != "79a1cc2c865653eafc03a39a2d81ed0f8bdafb86aa2cc7e9d3f17464faeb9536"
check_case "pinned ntt id is not the replaced 0.5.0 449925fb" 0 - - \
  test "$pinned_ntt_package_id" != "449925fb667ed1169fac61165fa236a03cc798a6b873bbc0c605ea062961d62e"
check_case "pinned ntt id is not the replaced 0.5.0 build 9e08a8e6" 0 - - \
  test "${pinned_ntt_package_id:0:8}" != "9e08a8e6"
check_case "default ntt pin version" 0 - - test "$pinned_ntt_version" == "0.6.1"
check_case "pinned wormhole-genesis id is 0.2.1 8af94ec5" 0 - - \
  test "$pinned_genesis_package_id" == "8af94ec56cd660812074f23437bbd798e1c13c6be18f512437a413bd9775096c"
check_case "pinned wormhole-genesis version" 0 - - test "$pinned_genesis_version" == "0.2.1"
check_case "pinned wormhole-genesis id is not the replaced 0.1.0 7e9ca47a" 0 - - \
  test "$pinned_genesis_package_id" != "7e9ca47aadda9bb3828d37a2844b1ee57ae77fa3151f3aecb84d66267e6c44d4"
check_case "pinned wormhole-genesis id is not the replaced build a64efec9" 0 - - \
  test "$pinned_genesis_package_id" != "a64efec90ef24be6b6533a62d4aaf6863e0aa8fa05ffe3ed6735c0bab295bd81"
check_case "pinned wormhole-genesis id is not the replaced build cbb432d7" 0 - - \
  test "$pinned_genesis_package_id" != "cbb432d75935af10179d1916f315034e68fa61ad4d53d6f3389b581f30e27221"

readonly FIXTURE_CORE_PACKAGE_ID="1111111111111111111111111111111111111111111111111111111111111111"
readonly FIXTURE_PRIM_PACKAGE_ID="2222222222222222222222222222222222222222222222222222222222222222"
readonly FIXTURE_CORE_DIRECTORY="wormhole-core-0.7.0-$FIXTURE_CORE_PACKAGE_ID"
# DAR layout with empty DALFs: the functions under test read only the entry names.
mkdir -p "$work_dir/fixture-core-dar/META-INF" "$work_dir/fixture-core-dar/$FIXTURE_CORE_DIRECTORY"
touch "$work_dir/fixture-core-dar/META-INF/MANIFEST.MF" \
  "$work_dir/fixture-core-dar/$FIXTURE_CORE_DIRECTORY/$FIXTURE_CORE_DIRECTORY.dalf" \
  "$work_dir/fixture-core-dar/$FIXTURE_CORE_DIRECTORY/daml-prim-$FIXTURE_PRIM_PACKAGE_ID.dalf"
(cd "$work_dir/fixture-core-dar" && zip -qr "$work_dir/fixture-core.dar" .) || failure_count=$((failure_count + 1))
readonly FIXTURE_CORE_DAR="$work_dir/fixture-core.dar"

echo "not a zip archive" > "$work_dir/not-a-dar.dar"
check_case "fixture core main id" 0 "$FIXTURE_CORE_PACKAGE_ID" - dar_main_package_id "$FIXTURE_CORE_DAR"
check_case "fixture core main name and version" 0 "wormhole-core 0.7.0" - \
  dar_main_package_name_and_version "$FIXTURE_CORE_DAR"
check_case "core id of the fixture core DAR" 0 - - \
  check_core_package_id_of_dar "$FIXTURE_CORE_DAR" "$FIXTURE_CORE_PACKAGE_ID"
check_case "wrong expected core id" 1 - "$FIXTURE_CORE_PACKAGE_ID, expected $pinned_core_package_id" \
  check_core_package_id_of_dar "$FIXTURE_CORE_DAR" "$pinned_core_package_id"
readonly SPLICE_HOLDING_PACKAGE_ID="718a0f77e505a8de22f188bd4c87fe74101274e9d4cb1bfac7d09aec7158d35b"
check_case "main id of the vendored damlc DAR splice-api-token-holding-v1" 0 "$SPLICE_HOLDING_PACKAGE_ID" - \
  dar_main_package_id "$VENDORED_DAR_DIR/splice-api-token-holding-v1-1.0.0.dar"
check_case "main name and version of the vendored damlc DAR splice-api-token-holding-v1" 0 "splice-api-token-holding-v1 1.0.0" - \
  dar_main_package_name_and_version "$VENDORED_DAR_DIR/splice-api-token-holding-v1-1.0.0.dar"
check_case "no core DALF" 1 - "found 0" \
  check_core_package_id_of_dar "$VENDORED_DAR_DIR/splice-api-token-holding-v1-1.0.0.dar" "$pinned_core_package_id"
check_case "main id of a file that is not a DAR" 1 - "cannot list" dar_main_package_id "$work_dir/not-a-dar.dar"
check_case "core id of a file that is not a DAR" 1 - "cannot list" \
  check_core_package_id_of_dar "$work_dir/not-a-dar.dar" "$pinned_core_package_id"
check_case "package ids of the fixture core DAR" 0 "$FIXTURE_CORE_PACKAGE_ID"$'\n'"$FIXTURE_PRIM_PACKAGE_ID" - \
  dar_package_ids "$FIXTURE_CORE_DAR"

ntt_canton_dir="${NTT_REPO_DIR:-$REPOSITORY_ROOT/../ntt}/canton"
ntt_dar="$ntt_canton_dir/ntt/.daml/dist/ntt-$pinned_ntt_version.dar"
if [[ -f "$ntt_dar" ]]; then
  check_case "NTT $pinned_ntt_version DAR main id equals the pin" 0 "$pinned_ntt_package_id" - dar_main_package_id "$ntt_dar"
  check_case "NTT $pinned_ntt_version DAR main name and version" 0 "ntt $pinned_ntt_version" - \
    dar_main_package_name_and_version "$ntt_dar"
  check_case "NTT $pinned_ntt_version DAR carries exactly the pinned core" 0 - - \
    check_core_package_id_of_dar "$ntt_dar" "$pinned_core_package_id"
else
  echo "skip: $ntt_dar is absent; set NTT_REPO_DIR to an NTT checkout with built DARs"
fi
genesis_dar="$ntt_canton_dir/genesis/.daml/dist/wormhole-genesis-$pinned_genesis_version.dar"
if [[ -f "$genesis_dar" ]]; then
  check_case "wormhole-genesis DAR main id equals the pin" 0 "$pinned_genesis_package_id" - dar_main_package_id "$genesis_dar"
  check_case "wormhole-genesis DAR main name and version" 0 "wormhole-genesis $pinned_genesis_version" - \
    dar_main_package_name_and_version "$genesis_dar"
  check_case "wormhole-genesis DAR carries exactly the pinned core" 0 - - \
    check_core_package_id_of_dar "$genesis_dar" "$pinned_core_package_id"
  check_case "wormhole-genesis DAR carries exactly the pinned ntt" 0 "$pinned_ntt_package_id" - \
    dar_dependency_package_ids "$genesis_dar" ntt
else
  echo "skip: $genesis_dar is absent; set NTT_REPO_DIR to an NTT checkout with built DARs"
fi
localnet_core_dar="$REPOSITORY_ROOT/canton/core/.daml/dist/wormhole-core-$pinned_core_version.dar"
if [[ -f "$localnet_core_dar" ]]; then
  check_case "LocalNet core DAR main id equals the pin" 0 "$pinned_core_package_id" - dar_main_package_id "$localnet_core_dar"
else
  echo "skip: $localnet_core_dar is absent; run 'dpm build --all' in canton"
fi

if [[ "$failure_count" -ne 0 ]]; then
  echo "ntt_lib_test.sh: $failure_count failures" >&2
  exit 1
fi
echo "ntt_lib_test.sh: PASS"
