# Functions of disclosure_service_check.sh that read only their arguments. test/disclosure_service_offline_lib_test.sh
# tests them without LocalNet. Source this file; do not run it. Needs bash and jq.
# shellcheck shell=bash

# check_replay_digest <digest>: exactly 64 lowercase hex characters.
check_replay_digest() {
  if [[ ! "$1" =~ ^[0-9a-f]{64}$ ]]; then
    echo "digest is not 64 lowercase hex characters: '$1'" >&2
    return 1
  fi
}

# covering_replay_node_in_acs <replay-node-events-json> <consumer> <namespace> <digest>: port of
# Test.TestReplay.coveringNode. Prints the contract id of the one node of (consumer, namespace) whose prefix starts the
# digest. Fails unless exactly one node covers the digest (the partition invariant).
covering_replay_node_in_acs() {
  local replay_node_events="$1" consumer="$2" namespace="$3" digest="$4"
  check_replay_digest "$digest" || return 1
  jq -er --arg consumer "$consumer" --arg namespace "$namespace" --arg digest "$digest" '
    [.[] | select(.createArgument.consumer == $consumer and .createArgument.namespace == $namespace)
         | select((.createArgument.prefix | if type == "object" then .value else . end) as $prefix
                  | $digest | startswith($prefix))
         | .contractId]
    | if length == 1 then .[0] else error("partition invariant violated: \(length) covering nodes") end' \
    <<<"$replay_node_events"
}

# synthetic_digest_with_first_nibble <nibble>: a digest that starts with that nibble, then 63 nibbles of 7.
synthetic_digest_with_first_nibble() {
  if [[ ! "$1" =~ ^[0-9a-f]$ ]]; then
    echo "not one lowercase hex nibble: '$1'" >&2
    return 1
  fi
  printf '%s%s\n' "$1" "$(printf '%63s' '' | tr ' ' 7)"
}

# replay_node_query_path <consumer> <namespace> <digest>: the /v1/replay-node path with URI-encoded parameters.
replay_node_query_path() {
  check_replay_digest "$3" || return 1
  jq -rn --arg consumer "$1" --arg namespace "$2" --arg digest "$3" \
    '"/v1/replay-node?consumer=\($consumer | @uri)&namespace=\($namespace | @uri)&digest=\($digest)"'
}

# claimed_roots_node_query_path <consumer> <namespace>: the /v1/claimed-roots-node path with URI-encoded parameters.
claimed_roots_node_query_path() {
  jq -rn --arg consumer "$1" --arg namespace "$2" \
    '"/v1/claimed-roots-node?consumer=\($consumer | @uri)&namespace=\($namespace | @uri)"'
}

# check_lookup_response <response-json>: the body of a lookup route has the five fields with the expected types.
check_lookup_response() {
  jq -e 'type == "object"
    and (keys == ["activeAtOffset", "contractId", "createdEventBlob", "synchronizerId", "templateId"])
    and (.templateId | type == "string" and test("^[0-9a-f]{64}:[A-Za-z0-9_.]+:[A-Za-z0-9_]+$"))
    and (.contractId | type == "string" and test("^[0-9a-f]{66,}$"))
    and (.createdEventBlob | type == "string" and length > 0)
    and (.synchronizerId | type == "string" and length > 0)
    and (.activeAtOffset | type == "number" and . > 0)' <<<"$1" > /dev/null 2>&1 || {
    echo "lookup response does not have the five fields with the expected types: $(head -c 300 <<<"$1")" >&2
    return 1
  }
}

# lookup_response_disclosure <response-json>: the DisclosedContract JSON of a lookup response.
lookup_response_disclosure() {
  check_lookup_response "$1" || return 1
  jq -c '{templateId, contractId, createdEventBlob, synchronizerId}' <<<"$1"
}

# prometheus_metric_sum <metrics-text> <metric-name> [<label-text>]: the sum of the samples of that metric whose label
# set contains <label-text> (for example reason="pruned_offset"), as an integer. 0 when no sample matches.
prometheus_metric_sum() {
  local metrics_text="$1" metric_name="$2" label_text="${3:-}"
  if [[ ! "$metric_name" =~ ^[a-z_][a-z0-9_]*$ ]]; then
    echo "not a metric name: '$metric_name'" >&2
    return 1
  fi
  awk -v name="$metric_name" -v label="$label_text" '
    $1 ~ /^#/ { next }
    {
      sample_name = $1
      sub(/\{.*/, "", sample_name)
      if (sample_name != name) next
      if (label != "" && index($1, label) == 0) next
      total += $2
    }
    END { printf "%d\n", total }' <<<"$metrics_text"
}

# ledger_lag_summary <samples-jsonl>: {sampleCount, notReadyCount, maxLagSeconds, maxLagOffsets, readyzStatuses} of the
# idle lag samples. Each sample is {readyzHttpStatus, readyzStatus, lagSeconds, lagOffsets}.
ledger_lag_summary() {
  jq -sc '{sampleCount: length,
    notReadyCount: ([.[] | select(.readyzHttpStatus != 200)] | length),
    maxLagSeconds: ([.[].lagSeconds] | max // 0),
    maxLagOffsets: ([.[].lagOffsets] | max // 0),
    readyzStatuses: ([.[].readyzStatus] | unique)}' <<<"$1"
}

# check_idle_lag_summary <summary-json> <minimum-samples> <lag-bound-seconds>: enough samples, each ready, and the
# largest lag below the bound.
check_idle_lag_summary() {
  local summary="$1" minimum_samples="$2" lag_bound_seconds="$3"
  jq -e --argjson minimum "$minimum_samples" --argjson bound "$lag_bound_seconds" \
    '.sampleCount >= $minimum and .notReadyCount == 0 and .maxLagSeconds < $bound' <<<"$summary" > /dev/null
}

# hosting_change_outcome <readyz-http-status> <answers-match-ledger: true|false> <resnapshot-rebuilds> <stale-or-missing>
# Prints one of:
#   consistent                  ready, answers match the ledger, no resnapshot difference
#   drift_detected_and_rebuilt  ready, answers match the ledger, a resnapshot found a difference and rebuilt the index
#   drift_detected_not_ready    not ready after a resnapshot difference
#   not_ready                   not ready, no resnapshot difference
#   ready_with_wrong_answers    ready, an answer differs from the ledger
hosting_change_outcome() {
  local readyz_http_status="$1" answers_match="$2" resnapshot_rebuilds="$3" stale_or_missing="$4"
  if [[ ! "$readyz_http_status" =~ ^[0-9]{3}$ || ! "$resnapshot_rebuilds" =~ ^[0-9]+$ || ! "$stale_or_missing" =~ ^[0-9]+$ ]]; then
    echo "hosting_change_outcome: invalid argument" >&2
    return 2
  fi
  case "$answers_match" in true | false) ;; *) echo "hosting_change_outcome: answers-match is not true or false" >&2; return 2 ;; esac
  if [[ "$readyz_http_status" != 200 ]]; then
    if ((resnapshot_rebuilds > 0 || stale_or_missing > 0)); then echo drift_detected_not_ready; else echo not_ready; fi
  elif [[ "$answers_match" != true ]]; then
    echo ready_with_wrong_answers
  elif ((resnapshot_rebuilds > 0 || stale_or_missing > 0)); then
    echo drift_detected_and_rebuilt
  else
    echo consistent
  fi
}

# hosting_change_outcome_is_safe <outcome>: a ready instance serves only answers that match the ledger.
hosting_change_outcome_is_safe() {
  case "$1" in
    consistent | drift_detected_and_rebuilt | drift_detected_not_ready | not_ready) return 0 ;;
    ready_with_wrong_answers) return 1 ;;
    *) echo "unknown hosting change outcome: '$1'" >&2; return 2 ;;
  esac
}

# archived_replay_node_ids <transaction-json>: the contract ids of the ReplayNodes that the transaction archives (consuming
# exercises in the ledger-effects shape), one per line.
archived_replay_node_ids() {
  jq -r '[.events[] | .ExercisedEvent? // empty
      | select((.templateId | endswith(":Wormhole.Core.Replay:ReplayNode")) and .consuming == true) | .contractId]
    | .[]' <<<"$1"
}

# created_replay_node_count_in <transaction-json> <namespace>: the number of ReplayNodes that the transaction creates in
# that namespace. A split creates 16.
created_replay_node_count_in() {
  jq -er --arg namespace "$2" '[.events[] | .CreatedEvent? // empty
      | select((.templateId | endswith(":Wormhole.Core.Replay:ReplayNode")) and .createArgument.namespace == $namespace)]
    | length' <<<"$1"
}
