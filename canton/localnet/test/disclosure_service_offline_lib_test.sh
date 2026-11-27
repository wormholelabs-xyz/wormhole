#!/usr/bin/env bash
# Table-driven test of disclosure_service_offline_lib.sh. Needs bash and jq.
# Usage: canton/localnet/test/disclosure_service_offline_lib_test.sh
set -uo pipefail
LOCALNET_TEST_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
readonly LOCALNET_TEST_DIR
# shellcheck source=SCRIPTDIR/../disclosure_service_offline_lib.sh
source "$LOCALNET_TEST_DIR/../disclosure_service_offline_lib.sh"

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

# repeat_character <character> <count>
repeat_character() { printf "%${2}s" "" | tr ' ' "$1"; }
CONSUMER="Integrator::1220$(repeat_character a 64)"
OTHER_CONSUMER="Other::1220$(repeat_character b 64)"
DIGEST_A1="a1$(repeat_character 0 62)"
DIGEST_B2="b2$(repeat_character 0 62)"
readonly CONSUMER OTHER_CONSUMER DIGEST_A1 DIGEST_B2

# replay_node_event <contract-id> <consumer> <namespace> <prefix>
replay_node_event() {
  jq -cn --arg cid "$1" --arg consumer "$2" --arg namespace "$3" --arg prefix "$4" \
    '{contractId: $cid, createArgument: {consumer: $consumer, namespace: $namespace, prefix: $prefix}}'
}
# replay_node_event_with_variant_prefix <contract-id> <consumer> <namespace> <prefix>: the prefix as {value: <hex>}.
replay_node_event_with_variant_prefix() {
  replay_node_event "$@" | jq -c '.createArgument.prefix |= {value: .}'
}

# Node sets of the covering table, by name.
declare -A REPLAY_NODE_SETS=()
REPLAY_NODE_SETS[root-only]="$(replay_node_event root "$CONSUMER" ns "" | jq -sc .)"
REPLAY_NODE_SETS[split]="$(for nibble in 0 1 2 3 4 5 6 7 8 9 a b c d e f; do replay_node_event "child-$nibble" "$CONSUMER" ns "$nibble"; done \
  | jq -sc .)"
REPLAY_NODE_SETS[split-with-other-tries]="$(jq -c --argjson other "$(replay_node_event other-root "$OTHER_CONSUMER" ns "")" \
  --argjson otherNamespace "$(replay_node_event other-namespace-root "$CONSUMER" ns2 "")" '. + [$other, $otherNamespace]' \
  <<<"${REPLAY_NODE_SETS[split]}")"
REPLAY_NODE_SETS[forked]="$(jq -c --argjson parent "$(replay_node_event stale-parent "$CONSUMER" ns "")" '. + [$parent]' \
  <<<"${REPLAY_NODE_SETS[split]}")"
REPLAY_NODE_SETS[variant-prefix]="$(for nibble in a b; do replay_node_event_with_variant_prefix "child-$nibble" "$CONSUMER" ns "$nibble"; done \
  | jq -sc .)"
REPLAY_NODE_SETS[deep]="$({ replay_node_event deep-a1 "$CONSUMER" ns a1; replay_node_event deep-a0 "$CONSUMER" ns a0; } | jq -sc .)"

# Columns: case | node set | consumer | namespace | digest | expected status | expected output | stderr fragment
while IFS='|' read -r case_name node_set_name consumer namespace digest expected_status expected_output stderr_fragment; do
  check_case "covering_replay_node_in_acs: $case_name" "$expected_status" "$expected_output" "$stderr_fragment" \
    covering_replay_node_in_acs "${REPLAY_NODE_SETS[$node_set_name]}" "$consumer" "$namespace" "$digest"
done <<EOF_TABLE
root covers every digest|root-only|$CONSUMER|ns|$DIGEST_A1|0|root|-
child a covers a1 after a split|split|$CONSUMER|ns|$DIGEST_A1|0|child-a|-
child b covers b2 after a split|split|$CONSUMER|ns|$DIGEST_B2|0|child-b|-
other consumer and namespace are ignored|split-with-other-tries|$CONSUMER|ns|$DIGEST_B2|0|child-b|-
depth-2 node|deep|$CONSUMER|ns|$DIGEST_A1|0|deep-a1|-
prefix in variant form|variant-prefix|$CONSUMER|ns|$DIGEST_B2|0|child-b|-
no covering node|deep|$CONSUMER|ns|$DIGEST_B2|5|-|0 covering nodes
two covering nodes (stale parent)|forked|$CONSUMER|ns|$DIGEST_A1|5|-|2 covering nodes
unknown namespace|split|$CONSUMER|ns-unknown|$DIGEST_A1|5|-|0 covering nodes
digest with 63 nibbles|split|$CONSUMER|ns|${DIGEST_A1:1}|1|-|not 64 lowercase hex
digest with upper case|split|$CONSUMER|ns|A1${DIGEST_A1:2}|1|-|not 64 lowercase hex
EOF_TABLE

check_case "synthetic digest for nibble 0" 0 "0$(repeat_character 7 63)" - synthetic_digest_with_first_nibble 0
check_case "synthetic digest for nibble f" 0 "f$(repeat_character 7 63)" - synthetic_digest_with_first_nibble f
check_case "synthetic digest for two nibbles" 1 - "not one lowercase hex nibble" synthetic_digest_with_first_nibble ab
check_case "synthetic digest for upper case" 1 - "not one lowercase hex nibble" synthetic_digest_with_first_nibble A

check_case "replay_node_query_path encodes the party and the namespace" 0 \
  "/v1/replay-node?consumer=Integrator%3A%3A1220$(repeat_character a 64)&namespace=a%20b%2Fc&digest=$DIGEST_A1" - \
  replay_node_query_path "$CONSUMER" "a b/c" "$DIGEST_A1"
check_case "replay_node_query_path with a bad digest" 1 - "not 64 lowercase hex" replay_node_query_path "$CONSUMER" ns 00
check_case "claimed_roots_node_query_path encodes the parameters" 0 \
  "/v1/claimed-roots-node?consumer=Integrator%3A%3A1220$(repeat_character a 64)&namespace=x%26y" - \
  claimed_roots_node_query_path "$CONSUMER" "x&y"

VALID_CONTRACT_ID="00$(repeat_character c 66)"
readonly VALID_CONTRACT_ID
VALID_LOOKUP="$(jq -cn --arg cid "$VALID_CONTRACT_ID" --arg template "$(repeat_character d 64):Wormhole.Core.State:CoreState" \
  '{templateId: $template, contractId: $cid, createdEventBlob: "Q0FG", synchronizerId: "global-domain::1220ab", activeAtOffset: 42}')"
check_case "check_lookup_response of a valid body" 0 - - check_lookup_response "$VALID_LOOKUP"
check_case "lookup_response_disclosure drops activeAtOffset" 0 \
  "$(jq -c '{templateId, contractId, createdEventBlob, synchronizerId}' <<<"$VALID_LOOKUP")" - lookup_response_disclosure "$VALID_LOOKUP"
# Columns: case | jq filter that damages the valid body
while IFS='|' read -r case_name damage_filter; do
  check_case "check_lookup_response: $case_name" 1 - "expected types" check_lookup_response "$(jq -c "$damage_filter" <<<"$VALID_LOOKUP")"
done <<'EOF_TABLE'
extra field|. + {role: "CoreState"}
missing activeAtOffset|del(.activeAtOffset)
zero activeAtOffset|.activeAtOffset = 0
offset as text|.activeAtOffset = "42"
empty blob|.createdEventBlob = ""
template without a package id|.templateId = "Wormhole.Core.State:CoreState"
short contract id|.contractId = "00ab"
EOF_TABLE
check_case "check_lookup_response of an error body" 1 - "expected types" check_lookup_response '{"error":"not found","requestId":"r"}'

METRICS_TEXT='# HELP wormhole_canton_disclosure_index_rebuilds_total Core disclosure index rebuilds by reason
# TYPE wormhole_canton_disclosure_index_rebuilds_total counter
wormhole_canton_disclosure_index_rebuilds_total{reason="resnapshot_difference"} 2
wormhole_canton_disclosure_index_rebuilds_total{reason="pruned_offset"} 1
wormhole_canton_disclosure_index_offset 1.234567e+06
wormhole_canton_disclosure_index_offset_extra 9'
check_case "metric sum over all labels" 0 3 - prometheus_metric_sum "$METRICS_TEXT" wormhole_canton_disclosure_index_rebuilds_total
check_case "metric sum of one label" 0 2 - prometheus_metric_sum "$METRICS_TEXT" wormhole_canton_disclosure_index_rebuilds_total 'reason="resnapshot_difference"'
check_case "metric in exponent form, name is not a prefix match" 0 1234567 - prometheus_metric_sum "$METRICS_TEXT" wormhole_canton_disclosure_index_offset
check_case "metric with no sample" 0 0 - prometheus_metric_sum "$METRICS_TEXT" wormhole_canton_disclosure_stream_connected
check_case "metric name with a brace" 1 - "not a metric name" prometheus_metric_sum "$METRICS_TEXT" 'a{b'

READY_SAMPLES='{"readyzHttpStatus":200,"readyzStatus":"ready","lagSeconds":4.9,"lagOffsets":2}
{"readyzHttpStatus":200,"readyzStatus":"ready","lagSeconds":12.5,"lagOffsets":7}'
LAGGING_SAMPLES='{"readyzHttpStatus":200,"readyzStatus":"ready","lagSeconds":4.9,"lagOffsets":2}
{"readyzHttpStatus":503,"readyzStatus":"ledger_lag_above_bound","lagSeconds":61,"lagOffsets":40}'
check_case "ledger_lag_summary of ready samples" 0 \
  '{"sampleCount":2,"notReadyCount":0,"maxLagSeconds":12.5,"maxLagOffsets":7,"readyzStatuses":["ready"]}' - ledger_lag_summary "$READY_SAMPLES"
check_case "idle lag summary passes" 0 - - check_idle_lag_summary "$(ledger_lag_summary "$READY_SAMPLES")" 2 60
check_case "idle lag summary with too few samples" 1 - - check_idle_lag_summary "$(ledger_lag_summary "$READY_SAMPLES")" 3 60
check_case "idle lag summary with a lag at the bound" 1 - - check_idle_lag_summary "$(ledger_lag_summary "$READY_SAMPLES")" 2 12.5
check_case "idle lag summary with a 503 sample" 1 - - check_idle_lag_summary "$(ledger_lag_summary "$LAGGING_SAMPLES")" 2 120

# Columns: case | readyz HTTP status | answers match | rebuilds | stale or missing | expected outcome | safe status
while IFS='|' read -r case_name readyz_http_status answers_match rebuilds stale_or_missing expected_outcome safe_status; do
  check_case "hosting_change_outcome: $case_name" 0 "$expected_outcome" - \
    hosting_change_outcome "$readyz_http_status" "$answers_match" "$rebuilds" "$stale_or_missing"
  check_case "hosting_change_outcome_is_safe: $case_name" "$safe_status" - - hosting_change_outcome_is_safe "$expected_outcome"
done <<'EOF_TABLE'
ready and equal|200|true|0|0|consistent|0
ready after a rebuild|200|true|1|3|drift_detected_and_rebuilt|0
difference without a rebuild yet|200|true|0|2|drift_detected_and_rebuilt|0
not ready after a difference|503|false|1|0|drift_detected_not_ready|0
not ready, no difference|503|true|0|0|not_ready|0
ready with a wrong answer|200|false|0|0|ready_with_wrong_answers|1
ready with a wrong answer after a rebuild|200|false|2|5|ready_with_wrong_answers|1
EOF_TABLE
check_case "hosting_change_outcome with a bad status" 2 - "invalid argument" hosting_change_outcome 20 true 0 0
check_case "hosting_change_outcome with a bad flag" 2 - "not true or false" hosting_change_outcome 200 yes 0 0
check_case "hosting_change_outcome_is_safe of an unknown outcome" 2 - "unknown hosting change outcome" hosting_change_outcome_is_safe drifted

readonly CONSUMED_NODE_EVENT='{"ExercisedEvent": {"templateId": "pkg:Wormhole.Core.Replay:ReplayNode", "contractId": "parent", "choice": "ConsumeDigest", "consuming": true}}'
readonly CORE_STATE_EXERCISE='{"ExercisedEvent": {"templateId": "pkg:Wormhole.Core.State:CoreState", "contractId": "core", "choice": "VerifyAndConsumeVAA", "consuming": false}}'
SPLIT_TRANSACTION="$(jq -c --argjson consumed "$CONSUMED_NODE_EVENT" --argjson core "$CORE_STATE_EXERCISE" \
  '{events: ([$core, $consumed] + [range(16) | {CreatedEvent: {templateId: "pkg:Wormhole.Core.Replay:ReplayNode", contractId: "child-\(.)", createArgument: {namespace: "ns"}}}]
    + [{CreatedEvent: {templateId: "pkg:Wormhole.Core.Replay:ReplayNode", contractId: "claimed", createArgument: {namespace: "wormhole-core:replay-roots"}}}])}' <<<'null')"
check_case "archived_replay_node_ids of a split" 0 parent - archived_replay_node_ids "$SPLIT_TRANSACTION"
check_case "archived_replay_node_ids skips nonconsuming exercises" 0 "" - archived_replay_node_ids "{\"events\": [$CORE_STATE_EXERCISE]}"
check_case "created_replay_node_count_in of a split" 0 16 - created_replay_node_count_in "$SPLIT_TRANSACTION" ns
check_case "created_replay_node_count_in of another namespace" 0 1 - created_replay_node_count_in "$SPLIT_TRANSACTION" wormhole-core:replay-roots
check_case "created_replay_node_count_in with no match" 0 0 - created_replay_node_count_in "$SPLIT_TRANSACTION" other

# Columns: case | outcome | requirement | expected status
while IFS='|' read -r case_name outcome requirement expected_status; do
  check_case "check_hosting_change_outcome: $case_name" "$expected_status" - - check_hosting_change_outcome "$outcome" "$requirement"
done <<'EOF_TABLE'
consistent where consistent is required|consistent|consistent|0
not ready where consistent is required|not_ready|consistent|1
rebuilt where consistent is required|drift_detected_and_rebuilt|consistent|1
wrong answers where consistent is required|ready_with_wrong_answers|consistent|1
not ready where safe is required|not_ready|safe|0
wrong answers where safe is required|ready_with_wrong_answers|safe|1
EOF_TABLE
check_case "check_hosting_change_outcome with an unknown requirement" 2 - "unknown hosting change requirement" check_hosting_change_outcome consistent exact

CONTRACT_ID_A="00$(repeat_character a 66)"
CONTRACT_ID_B="00$(repeat_character b 66)"
check_case "check_answers_have_contract_ids of two answers" 0 - - check_answers_have_contract_ids "/v1/core-state $CONTRACT_ID_A
$DIGEST_A1 $CONTRACT_ID_B"
# Columns: case | answers text | stderr fragment
while IFS='|' read -r case_name answers_text stderr_fragment; do
  check_case "check_answers_have_contract_ids: $case_name" 1 - "$stderr_fragment" check_answers_have_contract_ids "${answers_text//CID/$CONTRACT_ID_A}"
done <<'EOF_TABLE'
no answers||no answers
empty contract id|/v1/core-state |line 1 has no contract id
null contract id|/v1/core-state null|line 1 has no contract id
short contract id|/v1/core-state 00ab|line 1 has no contract id
extra field|/v1/core-state CID extra|line 1 has no contract id
EOF_TABLE
check_case "check_answers_have_contract_ids: empty second line" 1 - "line 2 has no contract id" check_answers_have_contract_ids "/v1/core-state $CONTRACT_ID_A
$DIGEST_A1 "

GO_CONSTANTS_FILE="$work_dir/constants.go"
printf '%s\n' 'const lagBound = 3 * time.Minute' 'const pollInterval = 5 * time.Second' 'const retention = 2 * time.Hour' \
  'const overheadBytes = 1536' 'const derivedBytes = overheadBytes * 2' > "$GO_CONSTANTS_FILE"
check_case "go_duration_constant_seconds of minutes" 0 180 - go_duration_constant_seconds "$GO_CONSTANTS_FILE" lagBound
check_case "go_duration_constant_seconds of seconds" 0 5 - go_duration_constant_seconds "$GO_CONSTANTS_FILE" pollInterval
check_case "go_duration_constant_seconds of hours" 0 7200 - go_duration_constant_seconds "$GO_CONSTANTS_FILE" retention
check_case "go_duration_constant_seconds of a missing name" 1 - "no duration constant" go_duration_constant_seconds "$GO_CONSTANTS_FILE" lag
check_case "go_integer_constant" 0 1536 - go_integer_constant "$GO_CONSTANTS_FILE" overheadBytes
check_case "go_integer_constant of an expression" 1 - "no integer constant" go_integer_constant "$GO_CONSTANTS_FILE" derivedBytes

# The suite constants equal the Go constants of the service.
readonly SUITE_SCRIPT="$LOCALNET_TEST_DIR/../disclosure_service_check.sh"
readonly CANTONDISCLOSURE_DIR="$LOCALNET_TEST_DIR/../../../node/pkg/cantondisclosure"
check_case "LEDGER_LAG_BOUND_SECONDS equals DefaultLedgerLagBound" 0 \
  "$(go_duration_constant_seconds "$CANTONDISCLOSURE_DIR/served_index.go" DefaultLedgerLagBound)" - \
  shell_readonly_integer "$SUITE_SCRIPT" LEDGER_LAG_BOUND_SECONDS
check_case "INDEX_ENTRY_OVERHEAD_BYTES equals indexEntryOverheadBytes" 0 \
  "$(go_integer_constant "$CANTONDISCLOSURE_DIR/indexed_core_contract.go" indexEntryOverheadBytes)" - \
  shell_readonly_integer "$SUITE_SCRIPT" INDEX_ENTRY_OVERHEAD_BYTES
check_case "shell_readonly_integer of a missing name" 1 - "no readonly integer" shell_readonly_integer "$SUITE_SCRIPT" NO_SUCH_CONSTANT

if ((failure_count > 0)); then
  echo "disclosure_service_offline_lib_test: $failure_count failure(s)" >&2
  exit 1
fi
echo "disclosure_service_offline_lib_test: PASS"
