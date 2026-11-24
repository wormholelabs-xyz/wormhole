#!/usr/bin/env bash
# LocalNet suite of `guardiand canton-disclosure-service` (the core disclosure service). LocalNet only.
#
# Usage: disclosure_service_check.sh check [<current-set-key-indices>]
#        disclosure_service_check.sh hosting-change [<owner-name> <owner-name>]
#        disclosure_service_check.sh down
#   check           D1 to D5, D7 to D9 against a service that reads sv as guardianGovernance. The default key indices
#                   are 1 (the guardian set after gg_operator_independence.sh 0 1).
#   hosting-change  D6: replicates the guardian namespace parties to guardian4, removes app-user as a host, and compares
#                   service instances on sv, app-user and guardian4 with the ledger. Run it after check. It changes
#                   the hosting of guardianGovernance until a LocalNet reset.
#   down            Stops the service instances, removes the auth proxy containers and guardian4.
#
# Checks:
#   D1 bootstrap and /v1/readyz 200          D2 singletons equal the ACS of sv (contract id, blob, template)
#   D3 covering ReplayNode and claimed-roots node equal the ledger through a split (more than 128 digests)
#   D4 each served ReplayNode is active once the index offset is past the archive offset
#   D5 submissions with only served blobs: VerifyAndConsumeVAA, and ClaimReplayRoot by a party on app-user that is not a
#      stakeholder; stale blobs fail with LOCAL_VERDICT_INACTIVE_CONTRACTS
#   D6 hosting change (hosting-change): a ready instance serves the ledger; app-user reports read_party_not_hosted
#   D7 idle ledger lag, resnapshot metrics and the core package count
#   D8 restart rebuilds the same answers     D9 a small byte cap serves the singletons only
#
# The service binary is built from this checkout. The LocalNet Ledger API needs a JWT, and the service supports only
# OAuth2 over TLS or no authentication. An nginx container per participant (the pinned image of images.lock) therefore
# adds the LocalNet JWT of a read-only user to each gRPC call. The service dials that proxy with --unsafePlaintext.
#
# Evidence: $LOCALNET_STATE_DIR/evidence/disclosure-service/<subcommand>-<UTC time>/ has results.jsonl (one line for each
# check row), one JSON file for each measurement, and the log of each service instance.
# Precondition: gg_host.sh, custody_host.sh for feeCustody and nttCustody, and gg_operator_independence.sh 0 1 ran.
set -euo pipefail
# shellcheck source=ntt_lib.sh
source "$(dirname "${BASH_SOURCE[0]}")/ntt_lib.sh"
# shellcheck source=disclosure_service_offline_lib.sh
source "$LOCALNET_SCRIPT_DIR/disclosure_service_offline_lib.sh"

readonly SUITE_STATE_DIR="$LOCALNET_STATE_DIR/disclosure-service"
readonly SUITE_CONTEXT_FILE="$SUITE_STATE_DIR/check-context.json"
readonly GUARDIAND_BINARY="$SUITE_STATE_DIR/guardiand"
readonly SUITE_VAA_TOOL="$SUITE_STATE_DIR/canton_localnet_vaa"
readonly SUITE_GO_TOOLCHAIN="go1.26.10"
readonly READER_LEDGER_USER="core-disclosure-reader"
readonly AUTH_PROXY_CONTAINER_PREFIX="disclosure-auth-proxy"
readonly LOCALNET_NETWORK="localnet"
readonly SERVICE_LISTEN_PORT_BASE=7610
readonly SERVICE_STATUS_PORT_BASE=7630
readonly AUTH_PROXY_PORT_BASE=7690
readonly MAX_SERVICE_INSTANCES=8
readonly SERVICE_READY_SECONDS=180
readonly SERVICE_STOP_SECONDS=20
readonly AUTH_PROXY_READY_SECONDS=30
readonly LOOKUP_CATCH_UP_POLLS=120
readonly LOOKUP_CATCH_UP_POLL_SECONDS=0.5
readonly RESNAPSHOT_INTERVAL="1m"
readonly RESNAPSHOT_INTERVAL_SECONDS=60
readonly LEDGER_LAG_BOUND_SECONDS=60
# Wormhole.Core.Replay.defaultSplitThreshold: the 129th consume in one namespace splits the root.
readonly REPLAY_SPLIT_THRESHOLD=128
readonly CONSUMES_AFTER_SPLIT=8
readonly MAX_CONSUMES=$((REPLAY_SPLIT_THRESHOLD + CONSUMES_AFTER_SPLIT))
readonly IDLE_LAG_MINIMUM_SECONDS=300
readonly IDLE_LAG_SECONDS="${DISCLOSURE_IDLE_LAG_SECONDS:-$IDLE_LAG_MINIMUM_SECONDS}"
readonly IDLE_LAG_SAMPLE_SECONDS=5
readonly HOSTING_SETTLE_SECONDS=$((RESNAPSHOT_INTERVAL_SECONDS * 3))
readonly HOSTING_CONSUMES_AFTER_CHANGE=3
readonly INDEX_ENTRY_OVERHEAD_BYTES=1536
readonly INACTIVE_CONTRACT_ERROR="LOCAL_VERDICT_INACTIVE_CONTRACTS"
readonly REPLAY_NODE_SUFFIX=":Wormhole.Core.Replay:ReplayNode"
readonly APPROVED_CORE_PACKAGES_FILE="$REPO_ROOT/node/pkg/cantonclient/approved_core_packages.json"
NGINX_IMAGE="$(awk '$1 == "nginx/nginx-unprivileged" {print $1 ":" $2 "@" $3}' "$LOCALNET_SCRIPT_DIR/images.lock")"
readonly NGINX_IMAGE
[[ "$NGINX_IMAGE" =~ @sha256:[0-9a-f]{64}$ ]] || fail "no digest-pinned nginx image in images.lock"
(( IDLE_LAG_SECONDS >= IDLE_LAG_MINIMUM_SECONDS && IDLE_LAG_SECONDS <= 3600 )) \
  || fail "DISCLOSURE_IDLE_LAG_SECONDS must be $IDLE_LAG_MINIMUM_SECONDS to 3600"

subcommand="${1:-}"
mkdir -p "$SUITE_STATE_DIR"
chmod 700 "$SUITE_STATE_DIR"
SUITE_EVIDENCE_DIR="$EVIDENCE_DIR/disclosure-service/${subcommand:-none}-$(date -u +%Y%m%dT%H%M%SZ)"
readonly SUITE_EVIDENCE_DIR
SUITE_RESULTS_FILE="$SUITE_EVIDENCE_DIR/results.jsonl"
readonly SUITE_RESULTS_FILE

# Service instances that this shell started: name -> index, validator.
declare -A SERVICE_INDEX_OF=()
declare -A SERVICE_VALIDATOR_OF=()
STARTED_SERVICE_INSTANCES=()
STARTED_AUTH_PROXY_VALIDATORS=()
check_passes=0
check_failures=0

# record_check <check-id> <PASS|FAIL|OBSERVED|NOT_TESTED> <description> [<detail-json>]
record_check() {
  local check_id="$1" outcome="$2" description="$3" detail="${4:-null}"
  jq -cn --arg check "$check_id" --arg outcome "$outcome" --arg description "$description" --argjson detail "$detail" \
    --arg recordedAt "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
    '{check: $check, outcome: $outcome, description: $description, detail: $detail, recordedAt: $recordedAt}' >> "$SUITE_RESULTS_FILE"
  case "$outcome" in
    PASS) check_passes=$((check_passes + 1)); echo "PASS $check_id: $description" ;;
    FAIL) check_failures=$((check_failures + 1)); echo "FAIL $check_id: $description" >&2 ;;
    OBSERVED | NOT_TESTED) echo "$outcome $check_id: $description" ;;
    *) fail "record_check: unknown outcome $outcome" ;;
  esac
}

# check_or_record <check-id> <description> <command...>: PASS when the command succeeds, FAIL otherwise.
check_or_record() {
  local check_id="$1" description="$2"
  shift 2
  if "$@"; then record_check "$check_id" PASS "$description"; else record_check "$check_id" FAIL "$description"; fi
}

# write_evidence_json <file-name> <json>
write_evidence_json() {
  jq . <<<"$2" > "$SUITE_EVIDENCE_DIR/$1"
}

# --- build ---

build_suite_binaries() {
  (cd "$REPO_ROOT/node" && GOTOOLCHAIN="$SUITE_GO_TOOLCHAIN" GOFLAGS=-mod=readonly GOPROXY=off \
    go build -o "$GUARDIAND_BINARY" . )
  (cd "$REPO_ROOT/node" && GOTOOLCHAIN="$SUITE_GO_TOOLCHAIN" GOFLAGS=-mod=readonly GOPROXY=off \
    go build -o "$SUITE_VAA_TOOL" ./hack/canton_localnet_vaa)
  echo "built $GUARDIAND_BINARY and $SUITE_VAA_TOOL at commit $(git -C "$REPO_ROOT" describe --always --dirty)"
}

# suite_vaa <canton_localnet_vaa flags...>: generate_vaa of gg_lib.sh with the prebuilt tool.
suite_vaa() {
  "$SUITE_VAA_TOOL" "$@"
}

# --- read-only ledger user and auth proxy ---

# ensure_reader_ledger_user <validator>: READER_LEDGER_USER exists there with exactly CanReadAs(guardianGovernance).
ensure_reader_ledger_user() {
  local validator_name="$1" http_status actual_rights expected_rights
  http_status="$(curl_bounded -sS -o "$gg_work_dir/reader-user.out" -w '%{http_code}' -X POST \
    "$(json_api_url "$validator_name")/v2/users" -H "@$ADMIN_AUTH_HEADER" -H 'Content-Type: application/json' \
    -d "$(jq -n --arg id "$READER_LEDGER_USER" --arg gg "$GG_PARTY" '{
      user: {id: $id, primaryParty: "", isDeactivated: false, identityProviderId: ""},
      rights: [{kind: {CanReadAs: {value: {party: $gg}}}}]}')")"
  case "$http_status" in
    200 | 409) ;;
    *) fail "create user $READER_LEDGER_USER on $validator_name: HTTP $http_status $(cat "$gg_work_dir/reader-user.out")" ;;
  esac
  expected_rights="$(jq -cn --arg gg "$GG_PARTY" '[{right: "CanReadAs", party: $gg}]')"
  actual_rights="$(curl_bounded -sSf "$(json_api_url "$validator_name")/v2/users/$READER_LEDGER_USER/rights" -H "@$ADMIN_AUTH_HEADER" \
    | jq -c '[.rights[].kind | to_entries[0] | {right: .key, party: (.value.value.party // null)}] | sort_by(.right, .party)')"
  [[ "$actual_rights" == "$expected_rights" ]] \
    || fail "user $READER_LEDGER_USER on $validator_name has rights $actual_rights, expected $expected_rights"
}

auth_proxy_container_of() { echo "$AUTH_PROXY_CONTAINER_PREFIX-$1"; }

auth_proxy_port_of() { echo $((AUTH_PROXY_PORT_BASE + $(validator_port_prefix "$1"))); }

# participant_grpc_target_of <validator>: host:port of the Ledger API on the localnet network.
participant_grpc_target_of() {
  case "$1" in
    guardian4) echo "guardian4:$(grpc_ledger_api_port guardian4)" ;;
    *) echo "canton:$(grpc_ledger_api_port "$1")" ;;
  esac
}

container_exists() { docker container inspect "$1" > /dev/null 2>&1; }

# start_auth_proxy <validator>: nginx that forwards gRPC to the participant with the JWT of READER_LEDGER_USER.
# The configuration holds the LocalNet JWT of the `unsafe` development secret (token.sh).
start_auth_proxy() {
  local validator_name="$1" container_name proxy_port config_file reader_token waited_seconds
  container_name="$(auth_proxy_container_of "$validator_name")"
  proxy_port="$(auth_proxy_port_of "$validator_name")"
  ! container_exists "$container_name" || fail "$container_name exists; run disclosure_service_check.sh down first"
  ensure_reader_ledger_user "$validator_name"
  reader_token="$("$LOCALNET_SCRIPT_DIR/token.sh" "$READER_LEDGER_USER")"
  config_file="$SUITE_STATE_DIR/$container_name.conf"
  cat > "$config_file" <<EOF
server {
  listen $proxy_port;
  http2 on;
  client_body_timeout 3600s;
  location / {
    grpc_pass grpc://$(participant_grpc_target_of "$validator_name");
    grpc_set_header Authorization "Bearer $reader_token";
    grpc_connect_timeout 5s;
    grpc_read_timeout 3600s;
    grpc_send_timeout 3600s;
  }
}
EOF
  # Readable by the nginx container user 101.
  chmod 644 "$config_file"
  docker run -d --name "$container_name" --network "$LOCALNET_NETWORK" -p "$HOST_BIND_IP:$proxy_port:$proxy_port" \
    -v "$config_file:/etc/nginx/conf.d/default.conf:ro" "$NGINX_IMAGE" > /dev/null
  STARTED_AUTH_PROXY_VALIDATORS+=("$validator_name")
  for ((waited_seconds = 0; waited_seconds < AUTH_PROXY_READY_SECONDS; waited_seconds++)); do
    if nc -z -w 2 127.0.0.1 "$proxy_port" 2> /dev/null; then
      echo "auth proxy $container_name: 127.0.0.1:$proxy_port -> $(participant_grpc_target_of "$validator_name") as $READER_LEDGER_USER"
      return 0
    fi
    sleep 1
  done
  docker logs --tail 20 "$container_name" >&2 || true
  fail "auth proxy $container_name does not listen after ${AUTH_PROXY_READY_SECONDS}s"
}

remove_auth_proxy() {
  local container_name
  container_name="$(auth_proxy_container_of "$1")"
  if container_exists "$container_name"; then
    docker rm -f "$container_name" > /dev/null
    echo "removed $container_name"
  fi
}

# --- service instances ---

service_url_of() { echo "http://127.0.0.1:$((SERVICE_LISTEN_PORT_BASE + SERVICE_INDEX_OF[$1]))"; }
service_status_url_of() { echo "http://127.0.0.1:$((SERVICE_STATUS_PORT_BASE + SERVICE_INDEX_OF[$1]))"; }
service_pid_file_of() { echo "$SUITE_STATE_DIR/$1.pid"; }
service_log_of() { echo "$SUITE_EVIDENCE_DIR/service-$1.log"; }

service_pid_is_service() {
  [[ "$(ps -p "$1" -o command= 2> /dev/null)" == "$GUARDIAND_BINARY canton-disclosure-service"* ]]
}

# start_service_instance <name> <index> <validator> [extra service flags...]: starts the service with its own listen and
# status ports and waits until /v1/healthz answers. The auth proxy of the validator must run.
start_service_instance() {
  local name="$1" instance_index="$2" validator_name="$3" waited_seconds service_pid
  shift 3
  (( instance_index >= 0 && instance_index < MAX_SERVICE_INSTANCES )) || fail "service instance index $instance_index"
  SERVICE_INDEX_OF[$name]="$instance_index"
  SERVICE_VALIDATOR_OF[$name]="$validator_name"
  nohup "$GUARDIAND_BINARY" canton-disclosure-service \
    --cantonRPC "127.0.0.1:$(auth_proxy_port_of "$validator_name")" \
    --cantonReadAsParty "$GG_PARTY" \
    --unsafePlaintext \
    --listenAddress "127.0.0.1:$((SERVICE_LISTEN_PORT_BASE + instance_index))" \
    --statusAddr "127.0.0.1:$((SERVICE_STATUS_PORT_BASE + instance_index))" \
    --ledgerLagBound "${LEDGER_LAG_BOUND_SECONDS}s" \
    --resnapshotInterval "$RESNAPSHOT_INTERVAL" \
    "$@" >> "$(service_log_of "$name")" 2>&1 &
  service_pid=$!
  echo "$service_pid" > "$(service_pid_file_of "$name")"
  STARTED_SERVICE_INSTANCES+=("$name")
  for ((waited_seconds = 0; waited_seconds < SERVICE_READY_SECONDS; waited_seconds++)); do
    if curl_bounded -sSf -o /dev/null "$(service_url_of "$name")/v1/healthz" 2> /dev/null; then
      echo "service $name (pid $service_pid) on $(service_url_of "$name"), reads $validator_name"
      return 0
    fi
    service_pid_is_service "$service_pid" || { tail -n 20 "$(service_log_of "$name")" >&2; fail "service $name exited"; }
    sleep 1
  done
  fail "service $name has no /v1/healthz after ${SERVICE_READY_SECONDS}s"
}

# stop_service_instance <name>: SIGTERM, then SIGKILL after SERVICE_STOP_SECONDS. Prints the exit wait in seconds.
stop_service_instance() {
  local name="$1" service_pid waited_seconds kept_names=() started_name
  [[ -f "$(service_pid_file_of "$name")" ]] || return 0
  service_pid="$(cat "$(service_pid_file_of "$name")")"
  if service_pid_is_service "$service_pid"; then
    kill -TERM "$service_pid"
    for ((waited_seconds = 0; waited_seconds < SERVICE_STOP_SECONDS; waited_seconds++)); do
      service_pid_is_service "$service_pid" || break
      sleep 1
    done
    if service_pid_is_service "$service_pid"; then
      kill -KILL "$service_pid"
      echo "service $name (pid $service_pid) did not stop on SIGTERM; sent SIGKILL" >&2
    fi
  fi
  rm -f "$(service_pid_file_of "$name")"
  for started_name in "${STARTED_SERVICE_INSTANCES[@]}"; do
    [[ "$started_name" == "$name" ]] || kept_names+=("$started_name")
  done
  STARTED_SERVICE_INSTANCES=("${kept_names[@]}")
}

# stop_leftover_service_instances: instances of an earlier run that left a pid file.
stop_leftover_service_instances() {
  local pid_file service_pid
  for pid_file in "$SUITE_STATE_DIR"/*.pid; do
    [[ -f "$pid_file" ]] || continue
    service_pid="$(cat "$pid_file")"
    if service_pid_is_service "$service_pid"; then
      kill -TERM "$service_pid"
      echo "stopped leftover service pid $service_pid ($(basename "$pid_file" .pid))"
    fi
    rm -f "$pid_file"
  done
}

cleanup_suite() {
  local exit_status=$? name validator_name
  for name in "${STARTED_SERVICE_INSTANCES[@]}"; do
    stop_service_instance "$name" || true
  done
  for validator_name in "${STARTED_AUTH_PROXY_VALIDATORS[@]}"; do
    remove_auth_proxy "$validator_name" || true
  done
  rm -rf "$gg_work_dir"
  exit "$exit_status"
}

# service_get <name> <path>: the body, then the HTTP status on the last line.
service_get() {
  curl_bounded -sS -w '\n%{http_code}' "$(service_url_of "$1")$2"
}

# service_metrics <name>: the /metrics text of the instance.
service_metrics() {
  curl_bounded -sSf "$(service_status_url_of "$1")/metrics"
}

# service_readyz <name>: {httpStatus, status}.
service_readyz() {
  local response
  response="$(service_get "$1" /v1/readyz)" || { jq -cn '{httpStatus: 0, status: "unreachable"}'; return 0; }
  jq -cn --argjson httpStatus "$(tail -n 1 <<<"$response")" --arg body "$(sed '$d' <<<"$response")" \
    '{httpStatus: $httpStatus, status: ($body | fromjson? | .status // $body)}'
}

# await_service_ready <name> <check-id>: polls /v1/readyz each second until 200. Records the statuses it saw.
await_service_ready() {
  local name="$1" check_id="$2" waited_seconds readiness statuses="[]" started_seconds
  started_seconds="$(date +%s)"
  for ((waited_seconds = 0; waited_seconds < SERVICE_READY_SECONDS; waited_seconds++)); do
    readiness="$(service_readyz "$name")"
    statuses="$(jq -c --argjson readiness "$readiness" '. + [$readiness.status] | unique' <<<"$statuses")"
    if [[ "$(jq -r .httpStatus <<<"$readiness")" == 200 ]]; then
      record_check "$check_id" PASS "$name: /v1/readyz 200 $(jq -r .status <<<"$readiness") after $(( $(date +%s) - started_seconds ))s" \
        "$(jq -cn --argjson statuses "$statuses" --argjson seconds "$(( $(date +%s) - started_seconds ))" '{statusesSeen: $statuses, secondsToReady: $seconds}')"
      return 0
    fi
    sleep 1
  done
  record_check "$check_id" FAIL "$name: /v1/readyz is not 200 after ${SERVICE_READY_SECONDS}s" "$(jq -cn --argjson statuses "$statuses" '{statusesSeen: $statuses}')"
  tail -n 20 "$(service_log_of "$name")" >&2
  return 1
}

# service_lookup_at_offset <name> <path> <minimum-offset>: the 200 body once activeAtOffset is at least the offset.
# Polls LOOKUP_CATCH_UP_POLLS times. Fails on a non-200 answer after the last poll.
service_lookup_at_offset() {
  local name="$1" path="$2" minimum_offset="$3" poll response body
  for ((poll = 1; poll <= LOOKUP_CATCH_UP_POLLS; poll++)); do
    response="$(service_get "$name" "$path")" || response=$'\n000'
    body="$(sed '$d' <<<"$response")"
    if [[ "$(tail -n 1 <<<"$response")" == 200 ]] && check_lookup_response "$body" 2> /dev/null \
        && (( $(jq -r .activeAtOffset <<<"$body") >= minimum_offset )); then
      printf '%s\n' "$body"
      return 0
    fi
    sleep "$LOOKUP_CATCH_UP_POLL_SECONDS"
  done
  echo "$name GET $path: HTTP $(tail -n 1 <<<"$response") $(head -c 200 <<<"$body"), wanted 200 at offset $minimum_offset or later" >&2
  return 1
}

# --- ledger reads on sv ---

ledger_end_of() {
  curl_bounded -sSf "$(json_api_url "$1")/v2/state/ledger-end" -H "@$SCRIPT_AUTH_HEADER" | jq -er .offset
}

# instance_ledger_end_of <name>: the ledger end of the participant that the instance reads. Offsets are per participant.
instance_ledger_end_of() {
  ledger_end_of "${SERVICE_VALIDATOR_OF[$1]}"
}

# check_json_matches <jq-filter> <json>
check_json_matches() {
  jq -e "$1" <<<"$2" > /dev/null
}

# ledger_replay_nodes: the ReplayNode created events that guardianGovernance sees on sv.
ledger_replay_nodes() {
  active_contracts "$SUBMITTING_VALIDATOR" "$GG_PARTY" "$REPLAY_NODE_SUFFIX"
}

# ledger_singleton_with_blob <qualified-template-suffix>: {contractId, templateId, createdEventBlob, synchronizerId} of
# the one active contract of guardianGovernance on sv.
ledger_singleton_with_blob() {
  active_contracts_with_blobs "$SUBMITTING_VALIDATOR" "$GG_PARTY" "$1" \
    | jq -ec --arg gg "$GG_PARTY" '[.[] | select(.createArgument.guardianGovernance == $gg)]
        | if length == 1 then .[0] | {contractId, templateId, createdEventBlob, synchronizerId}
          else error("expected one active contract, found \(length)") end'
}

# --- D1, D2 ---

readonly SINGLETON_ROUTES=("/v1/core-state|:Wormhole.Core.State:CoreState" "/v1/emitter-registry|:Wormhole.Core.State:EmitterRegistry"
  "/v1/replay-root-registry|:Wormhole.Core.State:ReplayRootRegistry")

# compare_singletons_with_ledger <check-id> <name>: each singleton route equals the ACS of sv.
compare_singletons_with_ledger() {
  local check_id="$1" name="$2" route_entry route template_suffix ledger_offset served ledger comparison
  for route_entry in "${SINGLETON_ROUTES[@]}"; do
    IFS='|' read -r route template_suffix <<<"$route_entry"
    ledger_offset="$(instance_ledger_end_of "$name")"
    ledger="$(ledger_singleton_with_blob "$template_suffix")" || { record_check "$check_id" FAIL "$name $route: no single ledger contract"; continue; }
    served="$(service_lookup_at_offset "$name" "$route" "$ledger_offset")" || { record_check "$check_id" FAIL "$name $route: no answer"; continue; }
    comparison="$(jq -cn --argjson served "$served" --argjson ledger "$ledger" --arg route "$route" '{route: $route,
      servedContractId: $served.contractId, ledgerContractId: $ledger.contractId,
      contractIdEqual: ($served.contractId == $ledger.contractId),
      blobEqual: ($served.createdEventBlob == $ledger.createdEventBlob),
      servedTemplateId: $served.templateId, ledgerTemplateId: $ledger.templateId,
      templateIdEqual: ($served.templateId == $ledger.templateId),
      servedSynchronizerId: $served.synchronizerId, ledgerSynchronizerId: $ledger.synchronizerId,
      synchronizerIdEqual: ($served.synchronizerId == $ledger.synchronizerId),
      activeAtOffset: $served.activeAtOffset}')"
    if jq -e '.contractIdEqual and .blobEqual and .templateIdEqual and .synchronizerIdEqual' <<<"$comparison" > /dev/null; then
      record_check "$check_id" PASS "$name $route equals the ACS of sv (contract id, blob, template id, synchronizer id)" "$comparison"
    else
      record_check "$check_id" FAIL "$name $route differs from the ACS of sv" "$comparison"
    fi
  done
}

# --- D3, D4, D5 ---

# claim_replay_root_command <registry-cid> <claimed-roots-node-cid> <consumer> <namespace>
claim_replay_root_command() {
  exercise_command "$REPLAY_ROOT_REGISTRY_TEMPLATE" "$1" ClaimReplayRoot "$(jq -n --arg consumer "$3" --arg gg "$GG_PARTY" \
    --arg namespace "$4" --arg threshold "$REPLAY_SPLIT_THRESHOLD" --arg claimedRootsNode "$2" \
    '{consumer: $consumer, namespace: $namespace, splitThreshold: $threshold, payer: $consumer, coreStateCid: null,
      feeAllocation: null, expectedGuardianGovernance: $gg, claimedRootsNodeCid: $claimedRootsNode}')"
}

# served_claim_disclosures <name> <consumer> <namespace> <minimum-offset>: prints {registry, claimedRootsNode} bodies.
served_claim_disclosures() {
  local registry claimed_roots_node
  registry="$(service_lookup_at_offset "$1" /v1/replay-root-registry "$4")" || return 1
  claimed_roots_node="$(service_lookup_at_offset "$1" "$(claimed_roots_node_query_path "$2" "$3")" "$4")" || return 1
  jq -cn --argjson registry "$registry" --argjson claimedRootsNode "$claimed_roots_node" '{registry: $registry, claimedRootsNode: $claimedRootsNode}'
}

# submit_claim_response <validator> <command-id> <consumer> <namespace> <served-claim-json>: response and HTTP status.
submit_claim_response() {
  local served="$5"
  submit_local_response "$1" "$2" "$(json_array "\"$3\"")" \
    "$(claim_replay_root_command "$(jq -r .registry.contractId <<<"$served")" "$(jq -r .claimedRootsNode.contractId <<<"$served")" "$3" "$4")" \
    "$(json_array "$(lookup_response_disclosure "$(jq -c .registry <<<"$served")")" \
      "$(lookup_response_disclosure "$(jq -c .claimedRootsNode <<<"$served")")")"
}

# verify_and_consume_command <core-state-cid> <vaa-hex> <replay-node-cid>
verify_and_consume_command() {
  exercise_command "$CORE_STATE_TEMPLATE" "$1" VerifyAndConsumeVAA "$(jq -n --arg vaa "$2" --arg consumer "$integrator_party" \
    --arg namespace "$replay_namespace" --arg node "$3" \
    '{encodedVAA: {tag: "EncodedVaa", value: $vaa}, consumer: $consumer, namespace: $namespace, replayNodeCid: $node}')"
}

# submit_consume_response <command-id> <vaa-hex> <served-core-state-json> <served-replay-node-json>
submit_consume_response() {
  submit_local_response "$SUBMITTING_VALIDATOR" "$1" "$(json_array "\"$integrator_party\"")" \
    "$(verify_and_consume_command "$(jq -r .contractId <<<"$3")" "$2" "$(jq -r .contractId <<<"$4")")" \
    "$(json_array "$(lookup_response_disclosure "$3")" "$(lookup_response_disclosure "$4")")"
}

# message_vaa <sequence>: a message VAA of the current guardian set.
message_vaa() {
  suite_vaa -kind message -signing-set-index "$guardian_set_index" -signing-key-indices "$guardian_key_indices" -sequence "$1"
}

# claim_integrator_replay_root: ClaimReplayRoot of (integrator, namespace) with the served registry and claimed-roots
# node only. D3 claimed-roots rows and the first D5 row.
claim_integrator_replay_root() {
  local ledger_offset served ledger_claimed_roots_node response transaction archived_claimed_roots_node
  ledger_offset="$(ledger_end_of "$SUBMITTING_VALIDATOR")"
  served="$(served_claim_disclosures sv-main "$integrator_party" "$replay_namespace" "$ledger_offset")" \
    || { record_check D3 FAIL "no served claim disclosures"; return 1; }
  pre_claim_claimed_roots_node="$(jq -c .claimedRootsNode <<<"$served")"
  ledger_claimed_roots_node="$(claimed_roots_node_covering "$integrator_party" "$replay_namespace")"
  if [[ "$(jq -r .claimedRootsNode.contractId <<<"$served")" == "$ledger_claimed_roots_node" ]]; then
    record_check D3 PASS "/v1/claimed-roots-node equals the ledger covering node of claimedRootDigest(integrator, namespace)" \
      "$(jq -cn --arg cid "$ledger_claimed_roots_node" '{contractId: $cid}')"
  else
    record_check D3 FAIL "/v1/claimed-roots-node $(jq -r .claimedRootsNode.contractId <<<"$served"), ledger $ledger_claimed_roots_node"
  fi
  response="$(submit_claim_response "$SUBMITTING_VALIDATOR" "disclosure-claim-$run_label" "$integrator_party" "$replay_namespace" "$served")"
  if [[ "$(tail -n 1 <<<"$response")" != 200 ]]; then
    record_check D5 FAIL "ClaimReplayRoot with served blobs: HTTP $(tail -n 1 <<<"$response") $(sed '$d' <<<"$response" | head -c 300)"
    return 1
  fi
  transaction="$(sed '$d' <<<"$response" | jq -c .transaction)"
  record_check D5 PASS "ClaimReplayRoot by the integrator (no stakeholder of the registry and the claimed-roots node) with served blobs only" \
    "$(jq -c '{updateId, offset}' <<<"$transaction")"
  archived_claimed_roots_node="$(archived_replay_node_ids "$transaction" | tr '\n' ' ' | sed 's/ $//')"
  if [[ "$archived_claimed_roots_node" == "$(jq -r .claimedRootsNode.contractId <<<"$served")" ]]; then
    record_check D3 PASS "ClaimReplayRoot consumed the claimed-roots node that the service served"
  else
    record_check D3 FAIL "ClaimReplayRoot archived '$archived_claimed_roots_node', served $(jq -r .claimedRootsNode.contractId <<<"$served")"
  fi
  last_core_offset="$(jq -r .offset <<<"$transaction")"
}

# consume_until_split: VerifyAndConsumeVAA MAX_CONSUMES times with served blobs. Before each consume, the served covering
# node equals the ledger covering node and is active.
consume_until_split() {
  local consume_number vaa digest served_node served_core_state ledger_node response transaction created_count
  local lookup_mismatches=0 archived_served=0 rejected_consumes=0
  for ((consume_number = 1; consume_number <= MAX_CONSUMES; consume_number++)); do
    vaa="$(message_vaa "$((run_label * 1000 + consume_number))")"
    digest="$(jq -er .digestHex <<<"$vaa")"
    served_node="$(service_lookup_at_offset sv-main "$(replay_node_query_path "$integrator_party" "$replay_namespace" "$digest")" "$last_core_offset")" \
      || { record_check D3 FAIL "consume $consume_number: no served covering node"; return 1; }
    served_core_state="$(service_lookup_at_offset sv-main /v1/core-state "$last_core_offset")" \
      || { record_check D5 FAIL "consume $consume_number: no served CoreState"; return 1; }
    ledger_node="$(covering_replay_node_in_acs "$(ledger_replay_nodes)" "$integrator_party" "$replay_namespace" "$digest")" \
      || { record_check D3 FAIL "consume $consume_number: the ledger has no single covering node"; return 1; }
    if [[ "$(jq -r .contractId <<<"$served_node")" != "$ledger_node" ]]; then
      lookup_mismatches=$((lookup_mismatches + 1))
      echo "consume $consume_number: served $(jq -r .contractId <<<"$served_node"), ledger $ledger_node" >&2
    fi
    if grep -qxF "$(jq -r .contractId <<<"$served_node")" "$archived_node_ids_file"; then
      archived_served=$((archived_served + 1))
    fi
    if (( consume_number == REPLAY_SPLIT_THRESHOLD + 1 )); then
      pre_split_parent_node="$served_node"
    fi
    response="$(submit_consume_response "disclosure-consume-$run_label-$consume_number" "$(jq -r .vaaHex <<<"$vaa")" \
      "$served_core_state" "$served_node")"
    if [[ "$(tail -n 1 <<<"$response")" != 200 ]]; then
      rejected_consumes=$((rejected_consumes + 1))
      echo "consume $consume_number rejected: $(sed '$d' <<<"$response" | head -c 400)" >&2
      break
    fi
    transaction="$(sed '$d' <<<"$response" | jq -c .transaction)"
    last_core_offset="$(jq -r .offset <<<"$transaction")"
    archived_replay_node_ids "$transaction" >> "$archived_node_ids_file"
    printf '%s\n' "$digest" >> "$consumed_digests_file"
    created_count="$(created_replay_node_count_in "$transaction" "$replay_namespace")"
    if (( created_count == 16 )); then
      split_offset="$last_core_offset"
      split_parent_node_id="$(archived_replay_node_ids "$transaction" | head -n 1)"
      echo "consume $consume_number split ${split_parent_node_id:0:16} into 16 nodes at offset $split_offset"
    fi
    (( consume_number % 16 == 0 )) && echo "consumed $consume_number digests; last offset $last_core_offset"
  done
  write_evidence_json consume-flood.json "$(jq -cn --argjson consumes "$(wc -l < "$consumed_digests_file" | tr -d ' ')" \
    --argjson mismatches "$lookup_mismatches" --argjson archivedServed "$archived_served" --argjson rejected "$rejected_consumes" \
    --arg splitOffset "${split_offset:-}" --arg splitParent "${split_parent_node_id:-}" --argjson threshold "$REPLAY_SPLIT_THRESHOLD" \
    '{consumedDigests: $consumes, splitThreshold: $threshold, servedLedgerMismatches: $mismatches, archivedNodesServed: $archivedServed,
      rejectedConsumes: $rejected, splitOffset: $splitOffset, splitParentNode: $splitParent}')"
  check_or_record D5 "each of $MAX_CONSUMES VerifyAndConsumeVAA submissions with the served CoreState and covering node commits" \
    test "$rejected_consumes" -eq 0
  check_or_record D3 "before each consume the served covering node equals the ledger covering node ($lookup_mismatches mismatches)" \
    test "$lookup_mismatches" -eq 0
  check_or_record D3 "a split happened: more than $REPLAY_SPLIT_THRESHOLD digests in one namespace" test -n "${split_offset:-}"
  check_or_record D4 "no lookup served an archived node once the index reached the last consume ($archived_served)" \
    test "$archived_served" -eq 0
}

# compare_covering_nodes_after_split <check-id> <name>: for every consumed digest and one synthetic digest per first nibble,
# the served covering node equals the ledger covering node and is active. Writes covering-nodes-<name>.json.
compare_covering_nodes_after_split() {
  local check_id="$1" name="$2" ledger_offset replay_nodes digest nibble served ledger_node rows_file mismatches archived_rows
  rows_file="$gg_work_dir/covering-rows-$name.jsonl"
  : > "$rows_file"
  ledger_offset="$(instance_ledger_end_of "$name")"
  replay_nodes="$(ledger_replay_nodes)"
  for digest in $(cat "$consumed_digests_file"; for nibble in 0 1 2 3 4 5 6 7 8 9 a b c d e f; do synthetic_digest_with_first_nibble "$nibble"; done); do
    served="$(service_lookup_at_offset "$name" "$(replay_node_query_path "$integrator_party" "$replay_namespace" "$digest")" "$ledger_offset")" \
      || served='{"contractId": "none"}'
    ledger_node="$(covering_replay_node_in_acs "$replay_nodes" "$integrator_party" "$replay_namespace" "$digest" 2>&1)" || true
    jq -cn --arg digest "$digest" --arg served "$(jq -r .contractId <<<"$served")" --arg ledger "$ledger_node" \
      --argjson archived "$(grep -qxF "$(jq -r .contractId <<<"$served")" "$archived_node_ids_file" && echo true || echo false)" \
      '{digest: $digest, servedNode: $served, ledgerNode: $ledger, equal: ($served == $ledger), servedNodeIsArchived: $archived}' >> "$rows_file"
  done
  mismatches="$(jq -s '[.[] | select(.equal | not)] | length' "$rows_file")"
  archived_rows="$(jq -s '[.[] | select(.servedNodeIsArchived)] | length' "$rows_file")"
  write_evidence_json "covering-nodes-$name.json" "$(jq -sc --argjson offset "$ledger_offset" \
    '{ledgerOffset: $offset, rows: ., distinctServedNodes: ([.[].servedNode] | unique | length)}' "$rows_file")"
  check_or_record "$check_id" "$name: $(wc -l < "$rows_file" | tr -d ' ') digests on all 16 sides of the split, served covering node equals the ledger ($mismatches mismatches)" \
    test "$mismatches" -eq 0
  check_or_record D4 "$name: no served node is archived ($archived_rows rows), index offset past the split offset ${split_offset:-?}" \
    test "$archived_rows" -eq 0
}

# check_split_parent_is_not_served: D4 after the index offset passed the split offset.
check_split_parent_is_not_served() {
  local served
  [[ -n "${split_offset:-}" && -n "${split_parent_node_id:-}" ]] || { record_check D4 FAIL "no split recorded"; return 1; }
  served="$(service_lookup_at_offset sv-main /v1/core-state "$split_offset")" \
    || { record_check D4 FAIL "the index offset does not reach the split offset $split_offset"; return 1; }
  record_check D4 OBSERVED "index offset $(jq -r .activeAtOffset <<<"$served") is at or past the split offset $split_offset"
}

# check_stale_replay_node_fails: the parent node served before the split, then the fresh node.
check_stale_replay_node_fails() {
  local vaa served_core_state fresh_node response
  [[ -n "${pre_split_parent_node:-}" ]] || { record_check D5 FAIL "no node captured before the split"; return 1; }
  vaa="$(message_vaa "$((run_label * 1000 + 999))")"
  served_core_state="$(service_lookup_at_offset sv-main /v1/core-state "$last_core_offset")" || { record_check D5 FAIL "no served CoreState"; return 1; }
  response="$(submit_consume_response "disclosure-consume-stale-$run_label" "$(jq -r .vaaHex <<<"$vaa")" "$served_core_state" "$pre_split_parent_node")"
  write_evidence_json stale-replay-node-response.json "$(jq -cn --arg status "$(tail -n 1 <<<"$response")" --arg body "$(sed '$d' <<<"$response")" \
    --arg node "$(jq -r .contractId <<<"$pre_split_parent_node")" '{httpStatus: $status, staleNode: $node, body: ($body | fromjson? // $body)}')"
  if classify_submit_response "$response" "$INACTIVE_CONTRACT_ERROR"; then
    record_check D5 PASS "VerifyAndConsumeVAA with the node blob served before the split: $INACTIVE_CONTRACT_ERROR"
  else
    record_check D5 FAIL "VerifyAndConsumeVAA with the node blob served before the split: HTTP $(tail -n 1 <<<"$response") $(sed '$d' <<<"$response" | jq -c '{code, cause: (.cause // "" | .[0:200])}' 2> /dev/null || true)"
  fi
  fresh_node="$(service_lookup_at_offset sv-main "$(replay_node_query_path "$integrator_party" "$replay_namespace" "$(jq -r .digestHex <<<"$vaa")")" "$last_core_offset")" \
    || { record_check D5 FAIL "no fresh covering node"; return 1; }
  response="$(submit_consume_response "disclosure-consume-fresh-$run_label" "$(jq -r .vaaHex <<<"$vaa")" "$served_core_state" "$fresh_node")"
  if [[ "$(tail -n 1 <<<"$response")" == 200 ]]; then
    record_check D5 PASS "control: the same VAA with a fresh served node commits"
    last_core_offset="$(sed '$d' <<<"$response" | jq -r .transaction.offset)"
    archived_replay_node_ids "$(sed '$d' <<<"$response" | jq -c .transaction)" >> "$archived_node_ids_file"
    jq -r .digestHex <<<"$vaa" >> "$consumed_digests_file"
  else
    record_check D5 FAIL "control: fresh served node: HTTP $(tail -n 1 <<<"$response") $(sed '$d' <<<"$response" | head -c 300)"
  fi
}

# check_claim_from_app_user: a party on app-user that only reads served blobs claims a root, and a
# claim with the claimed-roots node served before the integrator claim fails.
check_claim_from_app_user() {
  local claimant_party served response stale_served
  claimant_party="$(allocate_local_party app-user "DisclosureClaimant-$run_label")"
  served="$(served_claim_disclosures sv-main "$claimant_party" "$replay_namespace" "$last_core_offset")" \
    || { record_check D5 FAIL "no served claim disclosures for the app-user claimant"; return 1; }
  stale_served="$(jq -c --argjson stale "$pre_claim_claimed_roots_node" '.claimedRootsNode = $stale' <<<"$served")"
  response="$(submit_claim_response app-user "disclosure-claim-stale-$run_label" "$claimant_party" "$replay_namespace-stale" "$stale_served")"
  write_evidence_json stale-claimed-roots-node-response.json "$(jq -cn --arg status "$(tail -n 1 <<<"$response")" --arg body "$(sed '$d' <<<"$response")" \
    '{httpStatus: $status, body: ($body | fromjson? // $body)}')"
  if classify_submit_response "$response" "$INACTIVE_CONTRACT_ERROR"; then
    record_check D5 PASS "ClaimReplayRoot on app-user with the claimed-roots node served before an earlier claim: $INACTIVE_CONTRACT_ERROR"
  else
    record_check D5 FAIL "stale claimed-roots node on app-user: HTTP $(tail -n 1 <<<"$response") $(sed '$d' <<<"$response" | head -c 300)"
  fi
  served="$(served_claim_disclosures sv-main "$claimant_party" "$replay_namespace" "$last_core_offset")" || return 1
  response="$(submit_claim_response app-user "disclosure-claim-app-user-$run_label" "$claimant_party" "$replay_namespace" "$served")"
  if [[ "$(tail -n 1 <<<"$response")" == 200 ]]; then
    record_check D5 PASS "ClaimReplayRoot on app-user by $claimant_party, no stakeholder of the registry or the claimed-roots node, with served blobs only"
  else
    record_check D5 FAIL "ClaimReplayRoot on app-user with served blobs: HTTP $(tail -n 1 <<<"$response") $(sed '$d' <<<"$response" | head -c 300)"
  fi
}

# --- D7 ---

# measure_idle_lag <name>: samples /v1/readyz and the lag gauges each IDLE_LAG_SAMPLE_SECONDS for IDLE_LAG_SECONDS while
# the suite sends only non-core traffic.
measure_idle_lag() {
  local name="$1" samples_file sample_count summary metrics_text readiness started_seconds
  samples_file="$SUITE_EVIDENCE_DIR/idle-lag-samples-$name.jsonl"
  : > "$samples_file"
  started_seconds="$(date +%s)"
  for ((sample_count = 0; sample_count < IDLE_LAG_SECONDS / IDLE_LAG_SAMPLE_SECONDS; sample_count++)); do
    sleep "$IDLE_LAG_SAMPLE_SECONDS"
    readiness="$(service_readyz "$name")"
    metrics_text="$(service_metrics "$name")" || metrics_text=""
    jq -cn --argjson readiness "$readiness" --argjson elapsed "$(( $(date +%s) - started_seconds ))" \
      --argjson lagSeconds "$(awk '$1 == "wormhole_canton_disclosure_ledger_lag_seconds" {print $2}' <<<"$metrics_text" | head -n 1 | grep -E '^[0-9.e+-]+$' || echo -1)" \
      --argjson lagOffsets "$(prometheus_metric_sum "$metrics_text" wormhole_canton_disclosure_ledger_lag_offsets)" \
      --argjson indexOffset "$(prometheus_metric_sum "$metrics_text" wormhole_canton_disclosure_index_offset)" \
      '{elapsedSeconds: $elapsed, readyzHttpStatus: $readiness.httpStatus, readyzStatus: $readiness.status,
        lagSeconds: $lagSeconds, lagOffsets: $lagOffsets, indexOffset: $indexOffset}' >> "$samples_file"
  done
  summary="$(ledger_lag_summary "$(cat "$samples_file")" | jq -c --argjson first "$(head -n 1 "$samples_file")" \
    --argjson last "$(tail -n 1 "$samples_file")" --argjson seconds "$IDLE_LAG_SECONDS" \
    '. + {measuredSeconds: $seconds, indexOffsetAdvance: ($last.indexOffset - $first.indexOffset)}')"
  write_evidence_json "idle-lag-$name.json" "$summary"
  echo "idle lag $name: $summary"
  if check_idle_lag_summary "$summary" $((IDLE_LAG_SECONDS / IDLE_LAG_SAMPLE_SECONDS)) "$LEDGER_LAG_BOUND_SECONDS"; then
    record_check D7 PASS "$name: ${IDLE_LAG_SECONDS}s without core traffic, /v1/readyz 200 at each sample, largest lag $(jq -r .maxLagSeconds <<<"$summary")s below ${LEDGER_LAG_BOUND_SECONDS}s" "$summary"
  else
    record_check D7 FAIL "$name: idle lag samples $(jq -c . <<<"$summary")" "$summary"
  fi
}

# resnapshot_metrics_json <name>: the resnapshot and rebuild counters, and the resnapshot log counts of the instance.
resnapshot_metrics_json() {
  local name="$1" metrics_text log_file
  metrics_text="$(service_metrics "$name")" || metrics_text=""
  log_file="$(service_log_of "$name")"
  jq -cn \
    --argjson missing "$(prometheus_metric_sum "$metrics_text" wormhole_canton_disclosure_resnapshot_differences_total 'kind="missing_from_index"')" \
    --argjson stale "$(prometheus_metric_sum "$metrics_text" wormhole_canton_disclosure_resnapshot_differences_total 'kind="stale_in_index"')" \
    --argjson contentMismatch "$(prometheus_metric_sum "$metrics_text" wormhole_canton_disclosure_resnapshot_differences_total 'kind="content_mismatch"')" \
    --argjson resnapshotFailures "$(prometheus_metric_sum "$metrics_text" wormhole_canton_disclosure_resnapshot_failures_total)" \
    --argjson rebuilds "$(prometheus_metric_sum "$metrics_text" wormhole_canton_disclosure_index_rebuilds_total)" \
    --argjson resnapshotRebuilds "$(prometheus_metric_sum "$metrics_text" wormhole_canton_disclosure_index_rebuilds_total 'reason="resnapshot_difference"')" \
    --argjson reconnects "$(prometheus_metric_sum "$metrics_text" wormhole_canton_disclosure_stream_reconnects_total)" \
    --argjson ignoredUnknownArchives "$(prometheus_metric_sum "$metrics_text" wormhole_canton_disclosure_index_ignored_unknown_archives)" \
    --argjson matchLogs "$(grep -c 'resnapshot matches the index' "$log_file" || true)" \
    --argjson differenceLogs "$(grep -c 'resnapshot differs from the index' "$log_file" || true)" \
    --argjson rebuildLogs "$(grep -c 'rebuilding the core disclosure index' "$log_file" || true)" \
    '{differences: {missingFromIndex: $missing, staleInIndex: $stale, contentMismatch: $contentMismatch},
      resnapshotFailures: $resnapshotFailures, rebuilds: $rebuilds, resnapshotRebuilds: $resnapshotRebuilds, streamReconnects: $reconnects,
      ignoredUnknownArchives: $ignoredUnknownArchives, resnapshotMatchLogs: $matchLogs, resnapshotDifferenceLogs: $differenceLogs,
      rebuildLogs: $rebuildLogs}'
}

check_resnapshots_match() {
  local name="$1" metrics
  metrics="$(resnapshot_metrics_json "$name")"
  write_evidence_json "resnapshot-$name.json" "$metrics"
  if jq -e '.resnapshotMatchLogs >= 2 and .differences.missingFromIndex == 0 and .differences.staleInIndex == 0
      and .differences.contentMismatch == 0 and .resnapshotFailures == 0 and .rebuilds == 0' <<<"$metrics" > /dev/null; then
    record_check D7 PASS "$name: $(jq -r .resnapshotMatchLogs <<<"$metrics") resnapshots match the index, no difference, no rebuild" "$metrics"
  else
    record_check D7 FAIL "$name: resnapshot metrics $(jq -c . <<<"$metrics")" "$metrics"
  fi
}

# check_core_package_count: D7 package-upgrade row. Needs a second approved core package.
check_core_package_count() {
  local approved_count served_packages
  approved_count="$(jq -er 'length' "$APPROVED_CORE_PACKAGES_FILE")"
  served_packages="$(for route_entry in "${SINGLETON_ROUTES[@]}"; do
      service_lookup_at_offset sv-main "${route_entry%%|*}" 0 | jq -r '.templateId | split(":")[0]'
    done | sort -u | jq -Rsc 'split("\n") | map(select(length > 0))')"
  if (( approved_count < 2 )); then
    record_check D7 NOT_TESTED "package upgrade: the approved list has $approved_count core package; served template package ids $served_packages" \
      "$(jq -cn --argjson approved "$(cat "$APPROVED_CORE_PACKAGES_FILE")" --argjson served "$served_packages" '{approvedCorePackages: $approved, servedTemplatePackageIds: $served}')"
  else
    record_check D7 NOT_TESTED "package upgrade: $approved_count approved core packages; the suite has no upgrade step"
  fi
}

# --- D8, D9 ---

# capture_answers <name>: the contract ids of each singleton route and of the covering node of each consumed digest.
capture_answers() {
  local name="$1" route_entry digest minimum_offset
  minimum_offset="$(instance_ledger_end_of "$name")"
  {
    for route_entry in "${SINGLETON_ROUTES[@]}"; do
      printf '%s %s\n' "${route_entry%%|*}" "$(service_lookup_at_offset "$name" "${route_entry%%|*}" "$minimum_offset" | jq -r .contractId)"
    done
    while IFS= read -r digest; do
      printf '%s %s\n' "$digest" "$(service_lookup_at_offset "$name" "$(replay_node_query_path "$integrator_party" "$replay_namespace" "$digest")" "$minimum_offset" | jq -r .contractId)"
    done < "$consumed_digests_file"
  }
}

check_restart_serves_same_answers() {
  local answers_before answers_after
  answers_before="$(capture_answers sv-main)"
  stop_service_instance sv-main
  start_service_instance sv-main 0 "$SUBMITTING_VALIDATOR"
  await_service_ready sv-main D8 || return 1
  answers_after="$(capture_answers sv-main)"
  printf '%s\n' "$answers_before" > "$SUITE_EVIDENCE_DIR/answers-before-restart.txt"
  printf '%s\n' "$answers_after" > "$SUITE_EVIDENCE_DIR/answers-after-restart.txt"
  check_or_record D8 "after a restart the service serves the same $(wc -l <<<"$answers_after" | tr -d ' ') answers" \
    test "$answers_before" == "$answers_after"
}

# check_small_byte_cap: a cap that holds exactly the three singletons plus one entry overhead.
check_small_byte_cap() {
  local singleton_bytes=0 route_entry blob_bytes byte_cap readiness response
  for route_entry in "${SINGLETON_ROUTES[@]}"; do
    blob_bytes="$(service_lookup_at_offset sv-main "${route_entry%%|*}" 0 | jq -r .createdEventBlob | base64 -d | wc -c | tr -d ' ')"
    singleton_bytes=$((singleton_bytes + blob_bytes + INDEX_ENTRY_OVERHEAD_BYTES))
  done
  byte_cap=$((singleton_bytes + INDEX_ENTRY_OVERHEAD_BYTES))
  start_service_instance byte-cap 1 "$SUBMITTING_VALIDATOR" --indexByteCapBytes "$byte_cap"
  await_service_ready byte-cap D9 || return 1
  readiness="$(service_readyz byte-cap)"
  check_or_record D9 "byte cap $byte_cap: /v1/readyz $(jq -c . <<<"$readiness"), expected 200 ready_byte_cap_reached" \
    check_json_matches '.httpStatus == 200 and .status == "ready_byte_cap_reached"' "$readiness"
  response="$(service_get byte-cap /v1/core-state)"
  check_or_record D9 "byte cap: /v1/core-state HTTP $(tail -n 1 <<<"$response")" test "$(tail -n 1 <<<"$response")" == 200
  response="$(service_get byte-cap "$(replay_node_query_path "$integrator_party" "$replay_namespace" "$(head -n 1 "$consumed_digests_file")")")"
  check_or_record D9 "byte cap: /v1/replay-node HTTP $(tail -n 1 <<<"$response"), expected 503" test "$(tail -n 1 <<<"$response")" == 503
  response="$(service_get byte-cap "$(claimed_roots_node_query_path "$integrator_party" "$replay_namespace")")"
  check_or_record D9 "byte cap: /v1/claimed-roots-node HTTP $(tail -n 1 <<<"$response"), expected 503" test "$(tail -n 1 <<<"$response")" == 503
  stop_service_instance byte-cap
}

# --- subcommands ---

write_check_context() {
  jq -cn --arg integrator "$integrator_party" --arg namespace "$replay_namespace" --arg setIndex "$guardian_set_index" \
    --arg keyIndices "$guardian_key_indices" --arg runLabel "$run_label" \
    --rawfile digests "$consumed_digests_file" --rawfile archived "$archived_node_ids_file" \
    '{integratorParty: $integrator, replayNamespace: $namespace, guardianSetIndex: $setIndex, guardianKeyIndices: $keyIndices,
      runLabel: $runLabel, consumedDigests: ($digests | split("\n") | map(select(length > 0))),
      archivedNodeIds: ($archived | split("\n") | map(select(length > 0)))}' > "$SUITE_CONTEXT_FILE"
}

read_check_context() {
  [[ -f "$SUITE_CONTEXT_FILE" ]] || fail "missing $SUITE_CONTEXT_FILE; run disclosure_service_check.sh check first"
  integrator_party="$(jq -er .integratorParty "$SUITE_CONTEXT_FILE")"
  replay_namespace="$(jq -er .replayNamespace "$SUITE_CONTEXT_FILE")"
  guardian_set_index="$(jq -er .guardianSetIndex "$SUITE_CONTEXT_FILE")"
  guardian_key_indices="$(jq -er .guardianKeyIndices "$SUITE_CONTEXT_FILE")"
  jq -r '.consumedDigests[]' "$SUITE_CONTEXT_FILE" > "$consumed_digests_file"
  jq -r '.archivedNodeIds[]' "$SUITE_CONTEXT_FILE" > "$archived_node_ids_file"
}

report_summary() {
  local subcommand_name="$1"
  jq -sc 'group_by(.check) | map({check: .[0].check, outcomes: (group_by(.outcome) | map({(.[0].outcome): length}) | add)})' \
    "$SUITE_RESULTS_FILE" > "$SUITE_EVIDENCE_DIR/summary.json"
  echo "== summary ($SUITE_EVIDENCE_DIR)"
  jq -r '.[] | "\(.check) \(.outcomes | to_entries | map("\(.key)=\(.value)") | join(" "))"' "$SUITE_EVIDENCE_DIR/summary.json"
  echo "disclosure_service_check.sh $subcommand_name: $check_passes passed, $check_failures failed"
  if (( check_failures > 0 )); then
    echo "disclosure_service_check.sh $subcommand_name: FAIL" >&2
    exit 1
  fi
  echo "disclosure_service_check.sh $subcommand_name: PASS"
}

prepare_run() {
  mkdir -p "$SUITE_EVIDENCE_DIR"
  : > "$SUITE_RESULTS_FILE"
  trap cleanup_suite EXIT
  stop_leftover_service_instances
  consumed_digests_file="$gg_work_dir/consumed-digests.txt"
  archived_node_ids_file="$gg_work_dir/archived-node-ids.txt"
  : > "$consumed_digests_file"
  : > "$archived_node_ids_file"
  run_label="$(date +%s)"
  last_core_offset=0
  echo "evidence $SUITE_EVIDENCE_DIR"
  build_suite_binaries
}

run_check() {
  guardian_key_indices="${1:-1}"
  [[ "$guardian_key_indices" =~ ^[0-9]+(,[0-9]+)*$ ]] || fail "key indices must be comma-separated integers: $guardian_key_indices"
  prepare_run
  guardian_set_index="$(active_contracts "$SUBMITTING_VALIDATOR" "$GG_PARTY" :Wormhole.Core.State:CoreState \
    | jq -er --arg gg "$GG_PARTY" '[.[] | select(.createArgument.guardianGovernance == $gg)]
        | if length == 1 then .[0].createArgument.guardianSetIndex else error("expected one CoreState, found \(length)") end')"
  integrator_party="$(allocate_local_party "$SUBMITTING_VALIDATOR" "DisclosureIntegrator-$run_label")"
  replay_namespace="disclosure-service-$run_label"
  echo "integrator $integrator_party, namespace $replay_namespace, guardian set $guardian_set_index keys $guardian_key_indices"

  echo "== D1. Bootstrap against $SUBMITTING_VALIDATOR as guardianGovernance"
  start_auth_proxy "$SUBMITTING_VALIDATOR"
  start_auth_proxy app-user
  start_service_instance sv-main 0 "$SUBMITTING_VALIDATOR"
  await_service_ready sv-main D1 || report_summary check

  echo "== D2. Singletons equal the ledger"
  compare_singletons_with_ledger D2 sv-main

  echo "== D3, D4, D5. Claim, consume until a split, compare covering nodes"
  claim_integrator_replay_root || report_summary check
  consume_until_split || true
  write_check_context
  check_split_parent_is_not_served || true
  compare_covering_nodes_after_split D3 sv-main
  check_stale_replay_node_fails || true
  check_claim_from_app_user || true
  write_check_context

  echo "== D7. Idle ledger lag for ${IDLE_LAG_SECONDS}s, resnapshots, core packages"
  measure_idle_lag sv-main
  check_resnapshots_match sv-main
  check_core_package_count

  echo "== D8. Restart"
  check_restart_serves_same_answers || true

  echo "== D9. Small byte cap"
  check_small_byte_cap || true

  echo "== D2 and D3 after the restart, on app-user"
  start_service_instance app-user-main 2 app-user
  await_service_ready app-user-main D1 || true
  compare_singletons_with_ledger D2 app-user-main
  compare_covering_nodes_after_split D3 app-user-main
  report_summary check
}

# hosting_answers_match_ledger <name>: singletons and covering nodes of the context digests equal the ledger of sv.
hosting_answers_match_ledger() {
  local name="$1" route_entry served ledger digest replay_nodes ledger_offset mismatches=0
  ledger_offset="$(ledger_end_of "$SUBMITTING_VALIDATOR")"
  for route_entry in "${SINGLETON_ROUTES[@]}"; do
    served="$(service_get "$name" "${route_entry%%|*}" | sed '$d' | jq -r '.contractId // "none"' 2> /dev/null || echo none)"
    ledger="$(ledger_singleton_with_blob "${route_entry##*|}" | jq -r .contractId)"
    [[ "$served" == "$ledger" ]] || { mismatches=$((mismatches + 1)); echo "$name ${route_entry%%|*}: served $served, ledger $ledger" >&2; }
  done
  replay_nodes="$(ledger_replay_nodes)"
  while IFS= read -r digest; do
    served="$(service_get "$name" "$(replay_node_query_path "$integrator_party" "$replay_namespace" "$digest")" | sed '$d' | jq -r '.contractId // "none"' 2> /dev/null || echo none)"
    ledger="$(covering_replay_node_in_acs "$replay_nodes" "$integrator_party" "$replay_namespace" "$digest")"
    [[ "$served" == "$ledger" ]] || mismatches=$((mismatches + 1))
  done < "$consumed_digests_file"
  echo "$name: $mismatches answers differ from the ledger of sv at offset $ledger_offset" >&2
  [[ "$mismatches" -eq 0 ]]
}

# record_hosting_outcome <name>
record_hosting_outcome() {
  local name="$1" readiness metrics answers_match=false outcome detail
  readiness="$(service_readyz "$name")"
  metrics="$(resnapshot_metrics_json "$name")"
  hosting_answers_match_ledger "$name" && answers_match=true
  outcome="$(hosting_change_outcome "$(jq -r .httpStatus <<<"$readiness")" "$answers_match" \
    "$(jq -r .resnapshotRebuilds <<<"$metrics")" "$(jq -r '.differences.missingFromIndex + .differences.staleInIndex' <<<"$metrics")")"
  detail="$(jq -cn --arg outcome "$outcome" --arg validator "${SERVICE_VALIDATOR_OF[$name]}" --argjson readiness "$readiness" \
    --argjson metrics "$metrics" --argjson answersMatch "$answers_match" \
    '{outcome: $outcome, validator: $validator, readiness: $readiness, answersMatchLedger: $answersMatch, resnapshot: $metrics}')"
  write_evidence_json "hosting-change-$name.json" "$detail"
  if hosting_change_outcome_is_safe "$outcome"; then
    record_check D6 PASS "$name on ${SERVICE_VALIDATOR_OF[$name]}: $outcome (readyz $(jq -c . <<<"$readiness"))" "$detail"
  else
    record_check D6 FAIL "$name on ${SERVICE_VALIDATOR_OF[$name]}: $outcome" "$detail"
  fi
}

# consume_after_hosting_change: core traffic after the change, with blobs from the sv instance.
consume_after_hosting_change() {
  local consume_number vaa served_node served_core_state response
  last_core_offset="$(ledger_end_of "$SUBMITTING_VALIDATOR")"
  for ((consume_number = 1; consume_number <= HOSTING_CONSUMES_AFTER_CHANGE; consume_number++)); do
    vaa="$(message_vaa "$((run_label * 1000 + consume_number))")"
    served_node="$(service_lookup_at_offset hosting-sv "$(replay_node_query_path "$integrator_party" "$replay_namespace" "$(jq -r .digestHex <<<"$vaa")")" "$last_core_offset")" || return 1
    served_core_state="$(service_lookup_at_offset hosting-sv /v1/core-state "$last_core_offset")" || return 1
    response="$(submit_consume_response "disclosure-hosting-consume-$run_label-$consume_number" "$(jq -r .vaaHex <<<"$vaa")" "$served_core_state" "$served_node")"
    [[ "$(tail -n 1 <<<"$response")" == 200 ]] || { echo "hosting consume $consume_number: $(sed '$d' <<<"$response" | head -c 300)" >&2; return 1; }
    last_core_offset="$(sed '$d' <<<"$response" | jq -r .transaction.offset)"
    jq -r .digestHex <<<"$vaa" >> "$consumed_digests_file"
  done
}

run_hosting_change() {
  local first_owner="${1:-owner1}" second_owner="${2:-owner2}" phase
  prepare_run
  read_check_context
  echo "integrator $integrator_party, namespace $replay_namespace; owners $first_owner $second_owner"

  echo "== D6. Instances on sv and app-user before the hosting change"
  start_auth_proxy "$SUBMITTING_VALIDATOR"
  start_auth_proxy app-user
  start_service_instance hosting-sv 3 "$SUBMITTING_VALIDATOR"
  start_service_instance hosting-app-user 4 app-user
  await_service_ready hosting-sv D6 || report_summary hosting-change
  await_service_ready hosting-app-user D6 || report_summary hosting-change

  echo "== D6. Replicate the guardian namespace parties to guardian4 (party_replication.sh)"
  "$LOCALNET_SCRIPT_DIR/guardian4.sh" start
  for phase in connect prepare complete; do
    "$LOCALNET_SCRIPT_DIR/party_replication.sh" "$phase" "$first_owner" "$second_owner"
  done
  start_auth_proxy guardian4
  start_service_instance hosting-guardian4 5 guardian4
  await_service_ready hosting-guardian4 D6 || true

  echo "== D6. Remove app-user as a host"
  REPLICATION_REMOVED_HOST=app-user "$LOCALNET_SCRIPT_DIR/party_replication.sh" remove-host "$first_owner" "$second_owner"
  if "$LOCALNET_SCRIPT_DIR/gg_hosting_change_check.sh" > "$SUITE_EVIDENCE_DIR/gg_hosting_change_check.log" 2>&1; then
    record_check D6 PASS "gg_hosting_change_check.sh: hosts sv, app-provider, guardian4"
  else
    record_check D6 FAIL "gg_hosting_change_check.sh failed; see gg_hosting_change_check.log"
  fi

  echo "== D6. Core traffic after the change, then ${HOSTING_SETTLE_SECONDS}s for resnapshots"
  if consume_after_hosting_change; then
    record_check D6 PASS "$HOSTING_CONSUMES_AFTER_CHANGE VerifyAndConsumeVAA after the change with blobs from hosting-sv"
  else
    record_check D6 FAIL "consumes after the hosting change"
  fi
  sleep "$HOSTING_SETTLE_SECONDS"
  for name in hosting-sv hosting-app-user hosting-guardian4; do
    record_hosting_outcome "$name"
  done
  check_or_record D6 "hosting-app-user: /v1/readyz $(service_readyz hosting-app-user | jq -c .), expected 503 read_party_not_hosted" \
    check_json_matches '.httpStatus == 503 and .status == "read_party_not_hosted"' "$(service_readyz hosting-app-user)"
  report_summary hosting-change
}

run_down() {
  local validator_name
  mkdir -p "$SUITE_EVIDENCE_DIR"
  : > "$SUITE_RESULTS_FILE"
  stop_leftover_service_instances
  for validator_name in sv app-provider app-user guardian4; do
    remove_auth_proxy "$validator_name"
  done
  if container_exists guardian4 || container_exists guardian4-postgres; then
    "$LOCALNET_SCRIPT_DIR/guardian4.sh" remove
  fi
  rm -f "$SUITE_STATE_DIR"/*.conf
}

case "$subcommand" in
  check) run_check "${2:-1}" ;;
  hosting-change) run_hosting_change "${2:-owner1}" "${3:-owner2}" ;;
  down) run_down ;;
  *) echo "usage: disclosure_service_check.sh check [<key-indices>] | hosting-change [<owner> <owner>] | down" >&2; exit 2 ;;
esac
