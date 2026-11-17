#!/usr/bin/env bash
# Table-driven test of emitter_id_scan.sh against emitter_id_scan_mock_api.py. Needs bash, curl, jq and python3.
# Usage: canton/tools/emitter_id_scan_test.sh
set -euo pipefail

TOOLS_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
readonly TOOLS_DIR
readonly GG="guardianGovernance::1220aa"
readonly OPERATOR="Operator::1220bb"
readonly OWNER_A="OwnerA::1220cc"
readonly OWNER_B="OwnerB::1220dd"
readonly ROGUE_GG="RogueGovernance::1220ee"
readonly CORE_V6="6666666666666666666666666666666666666666666666666666666666666666"
readonly CORE_V7="7777777777777777777777777777777777777777777777777777777777777777"
readonly CORE_V8="8888888888888888888888888888888888888888888888888888888888888888"
readonly CORE_TEST_PACKAGE="eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee"
readonly PARTICIPANT="PAR::1220ff"
readonly MOCK_READY_SECONDS=10

work_dir="$(mktemp -d)"
mock_pid=""
cleanup() {
  [[ -z "$mock_pid" ]] || kill "$mock_pid" 2>/dev/null || true
  rm -rf "$work_dir"
}
trap cleanup EXIT
printf 'unused-token\n' > "$work_dir/token"

# emitter_event <contract-id> <owner> <emitter-id> [<guardian-governance>] [<package-name>]: a CreatedEvent of a 0.7.0 Emitter.
emitter_event() {
  jq -nc --arg cid "$1" --arg owner "$2" --arg id "$3" --arg gg "${4:-$GG}" --arg packageName "${5:-wormhole-core}" \
    --arg operator "$OPERATOR" --arg template "$CORE_V7:Wormhole.Core.State:Emitter" \
    '{CreatedEvent: {contractId: $cid, templateId: $template, packageName: $packageName,
      createArgument: {operator: $operator, guardianGovernance: $gg, owner: $owner, emitterId: $id, sequence: "0"}}}'
}

# registry_event <contract-id> <next-id>: a CreatedEvent of a 0.8.0 EmitterRegistry.
registry_event() {
  jq -nc --arg cid "$1" --arg next "$2" --arg gg "$GG" --arg operator "$OPERATOR" \
    --arg template "$CORE_V8:Wormhole.Core.State:EmitterRegistry" \
    '{CreatedEvent: {contractId: $cid, templateId: $template, packageName: "wormhole-core",
      createArgument: {operator: $operator, guardianGovernance: $gg, registrationFee: "0", nextId: $next}}}'
}

# vetted_package <package-id> <package-name>: one VettedPackage of the participant.
vetted_package() {
  jq -nc --arg packageId "$1" --arg packageName "$2" '{packageId: $packageId, packageName: $packageName, packageVersion: "0.0.0"}'
}
only_core_v8_vetted="[$(vetted_package "$CORE_V8" wormhole-core)]"

# write_fixture <name> <rights-json> <updates-json> <active-events-json> [<response-delay-seconds>] [<vetted-packages-json>]
write_fixture() {
  jq -n --argjson rights "$2" --argjson updates "$3" --argjson active "$4" --argjson delaySeconds "${5:-0}" \
    --argjson vettedPackages "${6:-$only_core_v8_vetted}" --arg participantId "$PARTICIPANT" \
    '{rights: $rights, ledgerEnd: ([$updates[].offset] + [0] | max), updates: $updates,
      activeContracts: [$active[] | .CreatedEvent], delaySeconds: $delaySeconds,
      participantId: $participantId, vettedPackages: $vettedPackages}' > "$work_dir/$1.json"
}

# update_at <offset> <event-json>...: one transaction with these events.
update_at() {
  local offset="$1"
  shift
  jq -nc --argjson offset "$offset" --argjson events "$(printf '%s\n' "$@" | jq -sc .)" \
    '{offset: $offset, updateId: ("u" + ($offset | tostring)), events: $events}'
}

read_only_rights="$(jq -nc --arg gg "$GG" '[{kind: {CanReadAs: {value: {party: $gg}}}}]')"
submit_rights="$(jq -nc --arg gg "$GG" '[{kind: {CanReadAs: {value: {party: $gg}}}}, {kind: {CanActAs: {value: {party: $gg}}}}]')"

emitter_a0="$(emitter_event a0 "$OWNER_A" 0)"
emitter_a0_successor="$(emitter_event a0s "$OWNER_A" 0)"
emitter_b1="$(emitter_event b1 "$OWNER_B" 1)"
emitter_b7_archived="$(emitter_event b7 "$OWNER_B" 7)"
emitter_a0_duplicate="$(emitter_event a0dup "$OWNER_A" 0)"
emitter_rogue_gg="$(emitter_event rogue "$OWNER_A" 50 "$ROGUE_GG")"
registry_next_10="$(registry_event reg10 10)"
registry_next_0="$(registry_event reg0 0)"
registry_next_2="$(registry_event reg2 2)"

base_history="$(jq -sc . <(update_at 1 "$emitter_a0") <(update_at 2 "$emitter_b1") <(update_at 3 "$emitter_a0_successor"))"
write_fixture clean "$read_only_rights" "$base_history" "[$emitter_a0_successor,$emitter_b1]"
write_fixture duplicate "$read_only_rights" \
  "$(jq -c --argjson extra "$(update_at 4 "$emitter_a0_duplicate")" '. + [$extra]' <<< "$base_history")" \
  "[$emitter_a0_successor,$emitter_b1,$emitter_a0_duplicate]"
write_fixture archived_high_id "$read_only_rights" \
  "$(jq -c --argjson extra "$(update_at 4 "$emitter_b7_archived")" '. + [$extra]' <<< "$base_history")" \
  "[$emitter_a0_successor,$emitter_b1]"
write_fixture submit_right "$submit_rights" "$base_history" "[$emitter_a0_successor,$emitter_b1]"
write_fixture active_not_in_history "$read_only_rights" "$base_history" \
  "[$emitter_a0_successor,$emitter_b1,$(emitter_event planted "$OWNER_B" 2)]"
write_fixture registry_ahead "$read_only_rights" \
  "$(jq -c --argjson extra "$(update_at 4 "$registry_next_10")" '. + [$extra]' <<< "$base_history")" \
  "[$emitter_a0_successor,$emitter_b1,$registry_next_10]"
write_fixture registry_low "$read_only_rights" \
  "$(jq -c --argjson extra "$(update_at 4 "$registry_next_0")" '. + [$extra]' <<< "$base_history")" \
  "[$emitter_a0_successor,$emitter_b1,$registry_next_0]"
write_fixture registry_exact "$read_only_rights" \
  "$(jq -c --argjson extra "$(update_at 4 "$registry_next_2")" '. + [$extra]' <<< "$base_history")" \
  "[$emitter_a0_successor,$emitter_b1,$registry_next_2]"
write_fixture archived_registry_high "$read_only_rights" \
  "$(jq -c --argjson first "$(update_at 4 "$registry_next_10")" --argjson second "$(update_at 5 "$registry_next_2")" \
    '. + [$first, $second]' <<< "$base_history")" \
  "[$emitter_a0_successor,$emitter_b1,$registry_next_2]"
# write_history_emitter_fixture <name> <emitter-id>: the clean history plus an archived Emitter with this id.
write_history_emitter_fixture() {
  write_fixture "$1" "$read_only_rights" \
    "$(jq -c --argjson extra "$(update_at 4 "$(emitter_event "id$2" "$OWNER_B" "$2")")" '. + [$extra]' <<< "$base_history")" \
    "[$emitter_a0_successor,$emitter_b1]"
}
write_history_emitter_fixture emitter_id_max 9007199254740991
write_history_emitter_fixture emitter_id_above_max 9007199254740992
write_history_emitter_fixture emitter_id_int64_max 9223372036854775807
write_history_emitter_fixture emitter_id_leading_zero 01
write_history_emitter_fixture emitter_id_negative -1
emitter_lookalike_package="$(emitter_event lookalike "$OWNER_A" 50 "$GG" wormhole-core-lookalike)"
write_fixture lookalike_package_ignored "$read_only_rights" \
  "$(jq -c --argjson extra "$(update_at 4 "$emitter_lookalike_package")" '. + [$extra]' <<< "$base_history")" \
  "[$emitter_a0_successor,$emitter_b1,$emitter_lookalike_package]"
write_fixture slow_api "$read_only_rights" "$base_history" "[$emitter_a0_successor,$emitter_b1]" 3
write_fixture core_v6_also_vetted "$read_only_rights" "$base_history" "[$emitter_a0_successor,$emitter_b1]" 0 \
  "[$(vetted_package "$CORE_V8" wormhole-core),$(vetted_package "$CORE_V6" wormhole-core)]"
write_fixture core_v7_vetted "$read_only_rights" "$base_history" "[$emitter_a0_successor,$emitter_b1]" 0 \
  "[$(vetted_package "$CORE_V7" wormhole-core)]"
write_fixture core_not_vetted "$read_only_rights" "$base_history" "[$emitter_a0_successor,$emitter_b1]" 0 "[]"
write_fixture core_test_package_vetted "$read_only_rights" "$base_history" "[$emitter_a0_successor,$emitter_b1]" 0 \
  "[$(vetted_package "$CORE_V8" wormhole-core),$(vetted_package "$CORE_TEST_PACKAGE" wormhole-core-test)]"
write_fixture rogue_gg_ignored "$read_only_rights" \
  "$(jq -c --argjson extra "$(update_at 4 "$emitter_rogue_gg")" '. + [$extra]' <<< "$base_history")" \
  "[$emitter_a0_successor,$emitter_b1,$emitter_rogue_gg]"

start_mock() {
  local fixture_name="$1" waited
  rm -f "$work_dir/port"
  python3 -I "$TOOLS_DIR/emitter_id_scan_mock_api.py" "$work_dir/$fixture_name.json" "$work_dir/port" &
  mock_pid=$!
  for ((waited = 0; waited < MOCK_READY_SECONDS * 10; waited++)); do
    [[ -s "$work_dir/port" ]] && return 0
    sleep 0.1
  done
  echo "mock API did not start for $fixture_name" >&2
  return 1
}

stop_mock() {
  kill "$mock_pid" 2>/dev/null || true
  wait "$mock_pid" 2>/dev/null || true
  mock_pid=""
}

# Columns: case name | fixture | page size | begin offset | baseline | expected active registries | expected exit | expected output
#          | curl max time seconds (empty: the scan default) | expected core package id (empty: CORE_V8; "unset": not set)
test_cases=(
  "clean history passes|clean|200|0||0|0|\"firstId\":2"
  "duplicate active emitter fails|duplicate|200|0||0|1|duplicate (operator, owner, emitterId)"
  "archived higher id sets firstId|archived_high_id|200|0||0|0|\"firstId\":8"
  "page size 1 gives the same firstId|archived_high_id|1|0||0|0|\"firstId\":8"
  "a submit right fails|submit_right|200|0||0|1|the scan needs exactly"
  "active emitter outside history fails|active_not_in_history|200|0||0|1|not in the update history"
  "registry nextId above history fails|registry_ahead|200|0||1|1|nextId other than historyMaxEmitterId + 1"
  "registry nextId below history fails|registry_low|200|0||1|1|nextId other than historyMaxEmitterId + 1"
  "registry nextId equal to history max plus one passes|registry_exact|200|0||1|0|\"firstId\":2"
  "archived registry with a high nextId leaves firstId|archived_registry_high|200|0||1|0|\"firstId\":2"
  "unexpected active registry fails|registry_ahead|200|0||0|1|active EmitterRegistry count is not 0"
  "rogue guardianGovernance emitter is ignored|rogue_gg_ignored|200|0||0|0|\"firstId\":2"
  "begin offset without baseline is refused|clean|200|2||0|2|needs <baseline-max-emitter-id>"
  "begin offset with baseline keeps the baseline|clean|200|3|7|0|0|\"firstId\":8"
  "emitter id 2^53 - 1 gives an exact firstId|emitter_id_max|200|0||0|0|\"firstId\":9007199254740992"
  "emitter id 2^53 fails|emitter_id_above_max|200|0||0|1|outside 0..maxEmitterId"
  "emitter id 2^63 - 1 fails|emitter_id_int64_max|200|0||0|1|outside 0..maxEmitterId"
  "emitter id with a leading zero fails|emitter_id_leading_zero|200|0||0|1|outside 0..maxEmitterId"
  "negative emitter id fails|emitter_id_negative|200|0||0|1|outside 0..maxEmitterId"
  "baseline above maxEmitterId is refused|clean|200|3|9007199254740992|0|2|baseline"
  "look-alike package emitter is ignored|lookalike_package_ignored|200|0||0|0|\"firstId\":2"
  "a Ledger API response slower than the curl timeout fails|slow_api|200|0||0|1|timed out|1"
  "an older wormhole-core vetted next to the expected one fails|core_v6_also_vetted|200|0||0|1|unexpected vetted wormhole-core"
  "only an older wormhole-core vetted fails|core_v7_vetted|200|0||0|1|unexpected vetted wormhole-core"
  "expected wormhole-core not vetted fails|core_not_vetted|200|0||0|1|is not vetted"
  "a vetted wormhole-core-test package is ignored|core_test_package_vetted|200|0||0|0|\"firstId\":2"
  "EXPECTED_CORE_PACKAGE_ID is required|clean|200|0||0|2|EXPECTED_CORE_PACKAGE_ID||unset"
  "EXPECTED_CORE_PACKAGE_ID must be a package id|clean|200|0||0|2|EXPECTED_CORE_PACKAGE_ID||0b0b"
)

failed_cases=0
for test_case in "${test_cases[@]}"; do
  IFS='|' read -r case_name fixture_name page_size begin_offset baseline expected_registries expected_exit expected_output \
    curl_max_time_seconds expected_core_package_id <<< "$test_case"
  start_mock "$fixture_name"
  scan_arguments=("http://127.0.0.1:$(cat "$work_dir/port")" "$work_dir/token" "$GG" "$OPERATOR" "$begin_offset")
  [[ -z "$baseline" ]] || scan_arguments+=("$baseline")
  scan_environment=()
  case "$expected_core_package_id" in
    "") scan_environment+=("EXPECTED_CORE_PACKAGE_ID=$CORE_V8") ;;
    unset) ;;
    *) scan_environment+=("EXPECTED_CORE_PACKAGE_ID=$expected_core_package_id") ;;
  esac
  set +e
  env -u EXPECTED_CORE_PACKAGE_ID "${scan_environment[@]}" SCAN_PAGE_SIZE="$page_size" EXPECT_ACTIVE_REGISTRIES="$expected_registries" \
    CURL_MAX_TIME_SECONDS="${curl_max_time_seconds:-60}" \
    "$TOOLS_DIR/emitter_id_scan.sh" "${scan_arguments[@]}" > "$work_dir/out" 2>&1
  actual_exit=$?
  set -e
  stop_mock
  if [[ "$actual_exit" -eq "$expected_exit" ]] && grep -qF -- "$expected_output" "$work_dir/out"; then
    echo "ok   $case_name"
  else
    echo "FAIL $case_name: exit $actual_exit (expected $expected_exit), output:" >&2
    sed 's/^/     /' "$work_dir/out" >&2
    failed_cases=$((failed_cases + 1))
  fi
done
echo "emitter_id_scan_test: $((${#test_cases[@]} - failed_cases)) passed, $failed_cases failed"
[[ "$failed_cases" -eq 0 ]]
