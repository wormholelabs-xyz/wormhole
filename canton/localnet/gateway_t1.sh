#!/usr/bin/env bash
# T1: the NTT disclosure service on a separate gateway network, reaching the app-provider JSON Ledger API only
# through a mutual-TLS proxy. A relayer that is not a stakeholder completes Mint, Release and a Transfer with the
# disclosure sets of the service. Docker on LocalNet only.
#
# Usage: gateway_t1.sh up|check|down|ledger-user|check-relayer
#   up             Makes a throwaway CA and certificates, the read-only ledger user, the networks and three containers.
#   check          Runs the checks. Exits non-zero if one check fails.
#   down           Removes the three containers and the two networks. LocalNet keeps running.
#   ledger-user    Makes only the read-only ledger user and its token file $LOCALNET_STATE_DIR/gateway-t1/gateway.jwt.
#   check-relayer  Runs checks C3 to C8 only, against the service at GATEWAY_T1_SERVICE_URL.
#
# Layout:
#   host relayer -> 127.0.0.1:7599 -> edge (gateway-edge-net, gateway-net)
#     -> disclosure service (gateway-net only, internal) -> mutual TLS -> ledger proxy (gateway-net, localnet)
#     -> canton:3975 (app-provider JSON Ledger API)
# gateway-net is an internal network: on Docker Desktop, a container on a normal bridge network reaches the
# loopback-published LocalNet ports through host.docker.internal.
#
# Precondition: NTT_PIN_VERSION=0.4.0, ntt_burn_mint.sh and ntt_lock_unlock_gg.sh ran.
# DISCLOSURE_SERVICE_IMAGE names the local image built from canton/disclosure/service/Dockerfile of the NTT repo.
set -euo pipefail
# shellcheck source=ntt_lib.sh
source "$(dirname "${BASH_SOURCE[0]}")/ntt_lib.sh"

readonly GATEWAY_NETWORK="gateway-net"
readonly EDGE_NETWORK="gateway-edge-net"
readonly PARTICIPANT_NETWORK="localnet"
readonly PARTICIPANT_HOST="canton"
readonly PARTICIPANT_JSON_API_PORT=3975
readonly PARTICIPANT_LEDGER_API_PORT=3901
readonly PROXY_CONTAINER="t1-ledger-proxy"
readonly PROXY_PORT=7443
readonly SERVICE_CONTAINER="t1-disclosure-gateway"
readonly SERVICE_PORT=7599
readonly EDGE_CONTAINER="t1-gateway-edge"
readonly SERVICE_URL="${GATEWAY_T1_SERVICE_URL:-http://127.0.0.1:$SERVICE_PORT}"
readonly GATEWAY_LEDGER_USER="disclosure-gateway"
readonly GATEWAY_STATE_DIR="$LOCALNET_STATE_DIR/gateway-t1"
readonly CERT_DAYS=2
readonly HEALTHZ_MAX_POLLS=30
readonly PROBE_CONNECT_SECONDS=3
readonly FRESH_SET_MAX_POLLS=15
# The digest-pinned LocalNet nginx image of images.lock.
NGINX_IMAGE="$(awk '$1 == "nginx/nginx-unprivileged" {print $1 ":" $2 "@" $3}' "$LOCALNET_SCRIPT_DIR/images.lock")"
readonly NGINX_IMAGE
[[ "$NGINX_IMAGE" =~ @sha256:[0-9a-f]{64}$ ]] || fail "no digest-pinned nginx image in images.lock"

# Peers of ntt_burn_mint.sh.
readonly BURN_MINT_PEER_CHAIN_ID=2
readonly BURN_MINT_PEER_MANAGER_ADDRESS="000000000000000000000000e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0"
readonly BURN_MINT_PEER_TRANSCEIVER_ADDRESS="000000000000000000000000d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1"
readonly BURN_MINT_PEER_TOKEN_ADDRESS="000000000000000000000000c2c2c2c2c2c2c2c2c2c2c2c2c2c2c2c2c2c2c2c2"
readonly LOCK_UNLOCK_PEER_TOKEN_ADDRESS="000000000000000000000000cacacacacacacacacacacacacacacacacacacaca"
readonly PEER_RECIPIENT_ADDRESS="000000000000000000000000b9b9b9b9b9b9b9b9b9b9b9b9b9b9b9b9b9b9b9b9"
readonly DAML_DECIMAL_FRACTION_DIGITS=10
readonly MINT_TRIMMED_AMOUNT=250000000
readonly MINT_AMOUNT="2.5"
readonly LOCK_AMOUNT="5.0"
readonly LOCK_RAW_AMOUNT=50000000000
readonly RELEASE_AMOUNT="2.0"
readonly RELEASE_TRIMMED_AMOUNT=200000000
readonly SECOND_RELEASE_AMOUNT="1.0"
readonly SECOND_RELEASE_TRIMMED_AMOUNT=100000000
readonly TAP_AMOUNT_USD="10.0"
readonly OUTBOUND_NONCE=9
# Paths of the removed generic template route and a ledger path. Only /v1, /v1/healthz, /v1/readyz, /v1/managers
# and /v1/flows/{flow} answer (NTT disclosure service, canton/ntt-disclosure-sets ab36968).
readonly REMOVED_SERVICE_PATHS=(
  "/v1/disclosures"
  "/v1/disclosures?template=Splice.AmuletRules:AmuletRules"
  "/v1/disclosures?template=Wormhole.Ntt.Manager:NttManager"
  "/v2/state/active-contracts"
)
readonly SERVICE_ROUTES_JSON='["/v1","/v1/flows/{flow}","/v1/healthz","/v1/managers","/v1/readyz"]'

mkdir -p "$GATEWAY_STATE_DIR"
chmod 700 "$GATEWAY_STATE_DIR"

container_exists() { docker container inspect "$1" > /dev/null 2>&1; }

readonly SERVICE_CONTAINER_UID=65532
# restrict_secret_to_service_user <file>: mode 0600. The service refuses a secret that other users can read. Docker
# Desktop maps a bind mount to the container user; on Linux the file must belong to the distroless nonroot user.
restrict_secret_to_service_user() {
  chmod 600 "$1"
  if [[ "$(uname -s)" == Linux ]]; then
    chown "$SERVICE_CONTAINER_UID:$SERVICE_CONTAINER_UID" "$1" \
      || fail "cannot give $1 to uid $SERVICE_CONTAINER_UID; run gateway_t1.sh up as root"
  fi
}
network_exists() { docker network inspect "$1" > /dev/null 2>&1; }

# --- up ---

make_certificates() {
  local cert_dir="$GATEWAY_STATE_DIR/certs" ca_name
  mkdir -p "$cert_dir"
  (
    umask 077
    for ca_name in ca rogue-ca; do
      openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes -days "$CERT_DAYS" \
        -subj "/CN=t1-$ca_name" -keyout "$cert_dir/$ca_name.key" -out "$cert_dir/$ca_name.crt" 2> /dev/null
    done
    # issue_certificate <name> <ca-name> <extensions>
    issue_certificate() {
      openssl req -new -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes -subj "/CN=$1" \
        -keyout "$cert_dir/$1.key" -out "$cert_dir/$1.csr" 2> /dev/null
      openssl x509 -req -in "$cert_dir/$1.csr" -CA "$cert_dir/$2.crt" -CAkey "$cert_dir/$2.key" -CAcreateserial \
        -days "$CERT_DAYS" -extfile <(printf '%s\n' "$3") -out "$cert_dir/$1.crt" 2> /dev/null
      rm -f "$cert_dir/$1.csr"
    }
    issue_certificate "$PROXY_CONTAINER" ca "subjectAltName=DNS:$PROXY_CONTAINER
extendedKeyUsage=serverAuth"
    issue_certificate gateway-client ca "extendedKeyUsage=clientAuth"
    issue_certificate rogue-client rogue-ca "extendedKeyUsage=clientAuth"
  )
  # Readable by the nginx container user 101.
  chmod 644 "$cert_dir"/*.crt "$cert_dir/$PROXY_CONTAINER.key"
  restrict_secret_to_service_user "$cert_dir/gateway-client.key"
  openssl verify -CAfile "$cert_dir/ca.crt" "$cert_dir/$PROXY_CONTAINER.crt" "$cert_dir/gateway-client.crt"
  if openssl verify -CAfile "$cert_dir/ca.crt" "$cert_dir/rogue-client.crt" > /dev/null 2>&1; then
    fail "the rogue client certificate verifies against the T1 CA"
  fi
}

# ensure_gateway_ledger_user: the user exists on app-provider with exactly CanReadAs(gg) and CanReadAs(nttCustody).
ensure_gateway_ledger_user() {
  local json_api expected_rights actual_rights
  json_api="$(json_api_url app-provider)"
  expected_rights="$(jq -cn --arg gg "$GG_PARTY" --arg custody "$NTT_CUSTODY_PARTY" \
    '[{CanReadAs: $gg}, {CanReadAs: $custody}] | sort_by(.CanReadAs)')"
  if ! curl_bounded -sSf "$json_api/v2/users/$GATEWAY_LEDGER_USER" -H "@$ADMIN_AUTH_HEADER" > /dev/null 2>&1; then
    curl_bounded -sSf -X POST "$json_api/v2/users" -H "@$ADMIN_AUTH_HEADER" -H 'Content-Type: application/json' \
      -d "$(jq -n --arg id "$GATEWAY_LEDGER_USER" --arg gg "$GG_PARTY" --arg custody "$NTT_CUSTODY_PARTY" '{
        user: {id: $id, primaryParty: "", isDeactivated: false, identityProviderId: ""},
        rights: [{kind: {CanReadAs: {value: {party: $gg}}}}, {kind: {CanReadAs: {value: {party: $custody}}}}]}')" \
      > /dev/null
  fi
  actual_rights="$(curl_bounded -sSf "$json_api/v2/users/$GATEWAY_LEDGER_USER/rights" -H "@$ADMIN_AUTH_HEADER" \
    | jq -c '[.rights[].kind | to_entries[0] | {(.key): (.value.value.party // "")}] | sort_by(.CanReadAs)')"
  [[ "$actual_rights" == "$expected_rights" ]] \
    || fail "$GATEWAY_LEDGER_USER rights are $actual_rights, expected $expected_rights"
  echo "ledger user $GATEWAY_LEDGER_USER rights $actual_rights"
  (umask 077 && "$LOCALNET_SCRIPT_DIR/token.sh" "$GATEWAY_LEDGER_USER" > "$GATEWAY_STATE_DIR/gateway.jwt")
  restrict_secret_to_service_user "$GATEWAY_STATE_DIR/gateway.jwt"
}

write_nginx_configs() {
  cat > "$GATEWAY_STATE_DIR/ledger-proxy.conf" <<EOF
server {
  listen $PROXY_PORT ssl;
  ssl_protocols TLSv1.2 TLSv1.3;
  ssl_certificate /etc/t1/$PROXY_CONTAINER.crt;
  ssl_certificate_key /etc/t1/$PROXY_CONTAINER.key;
  ssl_client_certificate /etc/t1/ca.crt;
  ssl_verify_client on;
  ssl_verify_depth 1;
  client_max_body_size 1m;
  location / {
    proxy_pass http://$PARTICIPANT_HOST:$PARTICIPANT_JSON_API_PORT;
    proxy_http_version 1.1;
    proxy_connect_timeout 5s;
    proxy_read_timeout 60s;
  }
}
EOF
  cat > "$GATEWAY_STATE_DIR/edge.conf" <<EOF
server {
  listen $SERVICE_PORT;
  location / {
    limit_except GET { deny all; }
    proxy_pass http://$SERVICE_CONTAINER:$SERVICE_PORT;
    proxy_connect_timeout 5s;
    proxy_read_timeout 60s;
  }
}
EOF
  chmod 644 "$GATEWAY_STATE_DIR/ledger-proxy.conf" "$GATEWAY_STATE_DIR/edge.conf"
}

await_service_healthz() {
  local poll
  for (( poll = 1; poll <= HEALTHZ_MAX_POLLS; poll++ )); do
    if curl_bounded -sSf "$SERVICE_URL/v1/healthz" > "$gg_work_dir/healthz.json" 2> /dev/null; then
      echo "healthz $(cat "$gg_work_dir/healthz.json")"
      return 0
    fi
    sleep 1
  done
  docker logs --tail 30 "$SERVICE_CONTAINER" >&2 || true
  fail "the disclosure service is not healthy after $HEALTHZ_MAX_POLLS s"
}

gateway_up() {
  [[ -n "${DISCLOSURE_SERVICE_IMAGE:-}" ]] || fail "set DISCLOSURE_SERVICE_IMAGE to the local disclosure service image"
  docker image inspect "$DISCLOSURE_SERVICE_IMAGE" > /dev/null || fail "image $DISCLOSURE_SERVICE_IMAGE does not exist"
  for container_name in "$PROXY_CONTAINER" "$SERVICE_CONTAINER" "$EDGE_CONTAINER"; do
    ! container_exists "$container_name" || fail "$container_name exists; run gateway_t1.sh down first"
  done
  make_certificates
  ensure_gateway_ledger_user
  write_nginx_configs
  network_exists "$GATEWAY_NETWORK" || docker network create --internal "$GATEWAY_NETWORK" > /dev/null
  network_exists "$EDGE_NETWORK" || docker network create "$EDGE_NETWORK" > /dev/null

  docker run -d --name "$PROXY_CONTAINER" --network "$PARTICIPANT_NETWORK" \
    -p "127.0.0.1:$PROXY_PORT:$PROXY_PORT" \
    -v "$GATEWAY_STATE_DIR/ledger-proxy.conf:/etc/nginx/conf.d/default.conf:ro" \
    -v "$GATEWAY_STATE_DIR/certs:/etc/t1:ro" "$NGINX_IMAGE" > /dev/null
  docker network connect "$GATEWAY_NETWORK" "$PROXY_CONTAINER"

  docker run -d --name "$SERVICE_CONTAINER" --network "$GATEWAY_NETWORK" \
    -v "$GATEWAY_STATE_DIR/certs/ca.crt:/etc/disclosure/ca.crt:ro" \
    -v "$GATEWAY_STATE_DIR/certs/gateway-client.crt:/etc/disclosure/gateway-client.crt:ro" \
    -v "$GATEWAY_STATE_DIR/certs/gateway-client.key:/etc/disclosure/gateway-client.key:ro" \
    -v "$GATEWAY_STATE_DIR/gateway.jwt:/etc/disclosure/gateway.jwt:ro" \
    "$DISCLOSURE_SERVICE_IMAGE" \
    --json-api "https://$PROXY_CONTAINER:$PROXY_PORT" \
    --json-api-ca-file /etc/disclosure/ca.crt \
    --json-api-client-cert-file /etc/disclosure/gateway-client.crt \
    --json-api-client-key-file /etc/disclosure/gateway-client.key \
    --access-token-file /etc/disclosure/gateway.jwt \
    --guardian-governance-party "$GG_PARTY" \
    --ntt-custody-party "$NTT_CUSTODY_PARTY" \
    --behind-edge-proxy \
    --listen "0.0.0.0:$SERVICE_PORT" > /dev/null

  # nginx resolves its upstream at start: attach both networks before the start.
  docker create --name "$EDGE_CONTAINER" --network "$EDGE_NETWORK" -p "127.0.0.1:$SERVICE_PORT:$SERVICE_PORT" \
    -v "$GATEWAY_STATE_DIR/edge.conf:/etc/nginx/conf.d/default.conf:ro" "$NGINX_IMAGE" > /dev/null
  docker network connect "$GATEWAY_NETWORK" "$EDGE_CONTAINER"
  docker start "$EDGE_CONTAINER" > /dev/null

  await_service_healthz
  echo "gateway_t1.sh up: service $SERVICE_URL, image $DISCLOSURE_SERVICE_IMAGE"
}

gateway_down() {
  local container_name network_name
  for container_name in "$EDGE_CONTAINER" "$SERVICE_CONTAINER" "$PROXY_CONTAINER"; do
    if container_exists "$container_name"; then
      docker rm -f "$container_name" > /dev/null
      echo "removed container $container_name"
    fi
  done
  for network_name in "$GATEWAY_NETWORK" "$EDGE_NETWORK"; do
    if network_exists "$network_name"; then
      docker network rm "$network_name" > /dev/null
      echo "removed network $network_name"
    fi
  done
}

# --- check ---

check_failures=0
check_passes=0
pass() { echo "PASS: $*"; check_passes=$((check_passes + 1)); }
fail_check() { echo "FAIL: $*" >&2; check_failures=$((check_failures + 1)); }

# proxy_status <curl TLS flags...>: HTTP status of GET /v2/version through the ledger proxy, or "tls-failure".
proxy_status() {
  local http_status
  if http_status="$(curl_bounded -s -o "$gg_work_dir/proxy-body" -w '%{http_code}' \
    --resolve "$PROXY_CONTAINER:$PROXY_PORT:127.0.0.1" --cacert "$GATEWAY_STATE_DIR/certs/ca.crt" "$@" \
    "https://$PROXY_CONTAINER:$PROXY_PORT/v2/version")"; then
    echo "$http_status"
  else
    echo "tls-failure"
  fi
}

check_proxy_requires_client_certificate() {
  local cert_dir="$GATEWAY_STATE_DIR/certs" status
  echo "== C1. The ledger proxy requires a client certificate of the T1 CA"
  status="$(proxy_status --cert "$cert_dir/gateway-client.crt" --key "$cert_dir/gateway-client.key")"
  if [[ "$status" == "200" ]]; then pass "C1 gateway client certificate: HTTP 200"; else fail_check "C1 gateway client certificate: $status"; fi
  status="$(proxy_status)"
  if [[ "$status" == "400" ]] && grep -qF "No required SSL certificate was sent" "$gg_work_dir/proxy-body"; then
    pass "C1 no client certificate: HTTP 400 'No required SSL certificate was sent'"
  else
    fail_check "C1 no client certificate: $status $(head -c 200 "$gg_work_dir/proxy-body" 2> /dev/null)"
  fi
  status="$(proxy_status --cert "$cert_dir/rogue-client.crt" --key "$cert_dir/rogue-client.key")"
  if [[ "$status" == "400" ]] && grep -qF "SSL certificate error" "$gg_work_dir/proxy-body"; then
    pass "C1 client certificate of another CA: HTTP 400 'The SSL certificate error'"
  else
    fail_check "C1 client certificate of another CA: $status"
  fi
}

# probe_from_gateway_network <host> <port>: prints "open" or "closed" for a TCP connection from gateway-net.
probe_from_gateway_network() {
  if docker run --rm --network "$GATEWAY_NETWORK" --entrypoint sh "$NGINX_IMAGE" \
      -c "nc -z -w $PROBE_CONNECT_SECONDS '$1' '$2'" > /dev/null 2>&1; then
    echo open
  else
    echo closed
  fi
}

check_gateway_network_isolation() {
  local participant_ip service_networks target state
  echo "== C2. gateway-net reaches the participant only through the proxy"
  service_networks="$(docker inspect "$SERVICE_CONTAINER" --format '{{json .NetworkSettings.Networks}}' | jq -c 'keys')"
  if [[ "$service_networks" == "[\"$GATEWAY_NETWORK\"]" ]]; then
    pass "C2 $SERVICE_CONTAINER networks $service_networks"
  else
    fail_check "C2 $SERVICE_CONTAINER networks $service_networks"
  fi
  if [[ "$(docker network inspect "$GATEWAY_NETWORK" --format '{{.Internal}}')" == "true" ]]; then
    pass "C2 $GATEWAY_NETWORK is internal"
  else
    fail_check "C2 $GATEWAY_NETWORK is not internal"
  fi
  participant_ip="$(docker inspect "$PARTICIPANT_HOST" --format "{{(index .NetworkSettings.Networks \"$PARTICIPANT_NETWORK\").IPAddress}}")"
  for target in "$participant_ip:$PARTICIPANT_JSON_API_PORT" "$participant_ip:$PARTICIPANT_LEDGER_API_PORT" \
      "$PARTICIPANT_HOST:$PARTICIPANT_JSON_API_PORT" "host.docker.internal:$PARTICIPANT_JSON_API_PORT" \
      "host.docker.internal:$PARTICIPANT_LEDGER_API_PORT"; do
    state="$(probe_from_gateway_network "${target%:*}" "${target##*:}")"
    if [[ "$state" == "closed" ]]; then pass "C2 direct $target from $GATEWAY_NETWORK: closed"; else fail_check "C2 direct $target from $GATEWAY_NETWORK: $state"; fi
  done
  state="$(probe_from_gateway_network "$PROXY_CONTAINER" "$PROXY_PORT")"
  if [[ "$state" == "open" ]]; then pass "C2 control $PROXY_CONTAINER:$PROXY_PORT from $GATEWAY_NETWORK: open"; else fail_check "C2 control proxy: $state"; fi
}

# service_get <path-and-query>: prints the body, then the HTTP status on the last line.
service_get() {
  curl_bounded -sS -w '\n%{http_code}' "$SERVICE_URL$1"
}

# check_service_answers <check-name> <path> <expected-status> <jq-filter of the JSON body, or - for any body>
check_service_answers() {
  local check_name="$1" path="$2" expected_status="$3" body_filter="$4" response
  response="$(service_get "$path")"
  if [[ "$(tail -n 1 <<<"$response")" == "$expected_status" ]] \
      && { [[ "$body_filter" == "-" ]] || sed '$d' <<<"$response" | jq -e "$body_filter" > /dev/null 2>&1; }; then
    pass "C3 $check_name: GET $path HTTP $expected_status $(sed '$d' <<<"$response" | head -c 120)"
  else
    fail_check "C3 $check_name: GET $path HTTP $(tail -n 1 <<<"$response"), expected $expected_status: $(sed '$d' <<<"$response" | head -c 200)"
  fi
}

check_service_endpoints() {
  local removed_path
  echo "== C3. Service health, the route set and the register flow"
  check_service_answers "healthz through the proxy" /v1/healthz 200 '. == {status: "ok"}'
  check_service_answers "readyz through the proxy" /v1/readyz 200 '. == {status: "ready"}'
  # The index also lists the CIP-0056 registry routes, outside /v1.
  check_service_answers "the /v1 routes of the index are the flow routes" /v1 200 \
    "([.endpoints[].path | select(. == \"/v1\" or startswith(\"/v1/\"))] | sort) == $SERVICE_ROUTES_JSON"
  for removed_path in "${REMOVED_SERVICE_PATHS[@]}"; do
    check_service_answers "no route" "$removed_path" 404 -
  done
  check_service_answers "control managers" /v1/managers 200 '(.managers | length) >= 1'
  check_service_answers "register flow without --operator-party" /v1/flows/register 404 \
    '.error == "disclosure-service: flow register not enabled: the service has no operator party"'
}

# flow_set <flow> <query>: the flow response JSON. Fails unless HTTP 200.
flow_set() {
  local response
  response="$(service_get "/v1/flows/$1?$2")"
  if [[ "$(tail -n 1 <<<"$response")" != "200" ]]; then
    echo "flow $1: HTTP $(tail -n 1 <<<"$response") $(sed '$d' <<<"$response")" >&2
    return 1
  fi
  sed '$d' <<<"$response" | jq -c .
}

# flow_role_cid <flow-json> <role>: contract id of the one disclosure with that role.
flow_role_cid() {
  jq -er --arg role "$2" '[.disclosures[] | select(.role == $role) | .contractId]
    | if length == 1 then .[0] else error("expected one \($role), found \(length)") end' <<<"$1"
}

flow_disclosed_contracts() {
  jq -c '[.disclosures[] | {templateId, contractId, createdEventBlob, synchronizerId}]' <<<"$1"
}

# check_flow_missing_roles <flow-json> <expected-roles-json>
check_flow_missing_roles() {
  jq -e --argjson expected "$2" '([.missing[].role] | sort) == ($expected | sort)' <<<"$1" > /dev/null
}

check_relayer_is_not_stakeholder() {
  local suffix count
  for suffix in ":Wormhole.Ntt.Manager:NttManager" ":Wormhole.Core.State:CoreState" ":Wormhole.Ntt.Ledger:LockedLedger"; do
    count="$(active_contracts app-user "$relayer_party" "$suffix" | jq length)"
    if [[ "$count" == "0" ]]; then pass "C4 relayer sees 0 ${suffix##*:}"; else fail_check "C4 relayer sees $count ${suffix##*:}"; fi
  done
}

check_mint_through_service() {
  local manager_address user_party vaa digest flow manager_cid mint_transaction coins_before coins_after
  echo "== C5. (a) Mint on the burn/mint deployment with the service set"
  manager_address="$(ntt_contract burnMint | jq -er .managerAddress)"
  user_party="$(ntt_contract burnMint | jq -er .user)"
  vaa="$(ntt_transfer_vaa -sequence "$run_label" -emitter-chain "$BURN_MINT_PEER_CHAIN_ID" \
    -emitter-address "$BURN_MINT_PEER_TRANSCEIVER_ADDRESS" -source-manager "$BURN_MINT_PEER_MANAGER_ADDRESS" \
    -recipient-manager "$manager_address" -source-token "$BURN_MINT_PEER_TOKEN_ADDRESS" -recipient-party "$user_party" \
    -ntt-message-id "$run_label" -trimmed-decimals 8 -trimmed-amount "$MINT_TRIMMED_AMOUNT")"
  digest="$(jq -er .digestHex <<<"$vaa")"
  flow="$(flow_set mint "manager=$manager_address&digest=$digest&recipient=$user_party")" || { fail_check "C5 mint flow"; return; }
  echo "mint set roles $(jq -c '[.disclosures[].role]' <<<"$flow") missing $(jq -c '[.missing[].role]' <<<"$flow")"
  check_flow_missing_roles "$flow" '[]' || fail_check "C5 mint set has missing roles"
  manager_cid="$(flow_role_cid "$flow" NttManager)"
  coins_before="$(active_contracts app-user "$user_party" ":Token.CIP0056.Coin:Coin" | jq -c '[.[].createArgument.amount]')"
  if mint_transaction="$(submit_local app-user "t1-mint-$run_label" "$relayer_only" \
      "$(exercise_command "$NTT_MANAGER_TEMPLATE" "$manager_cid" Mint "$(jq -n --arg relayer "$relayer_party" \
        --arg coreState "$(flow_role_cid "$flow" CoreState)" --arg replayNode "$(flow_role_cid "$flow" "covering ReplayNode")" \
        --arg vaa "$(jq -r .vaaHex <<<"$vaa")" --argjson keys "$(vaa_pub_keys_json "$vaa")" --arg user "$user_party" \
        --arg preapproval "$(flow_role_cid "$flow" DepositPreapproval)" --argjson extraArgs "$EMPTY_EXTRA_ARGS" '{
          executor: $relayer, coreStateCid: $coreState, replayNodeCid: $replayNode, vaaBytes: $vaa, pubKeys: $keys,
          recipient: $user, depositPreapprovalCid: $preapproval, extraArgs: $extraArgs}')")" \
      "$(flow_disclosed_contracts "$flow")")"; then
    coins_after="$(active_contracts app-user "$user_party" ":Token.CIP0056.Coin:Coin" | jq -c '[.[].createArgument.amount]')"
    if (( $(json_decimal_array_total_units "$DAML_DECIMAL_FRACTION_DIGITS" "$coins_after") \
        - $(json_decimal_array_total_units "$DAML_DECIMAL_FRACTION_DIGITS" "$coins_before") \
        == $(decimal_to_units "$MINT_AMOUNT" "$DAML_DECIMAL_FRACTION_DIGITS") )); then
      pass "C5 Mint update $(jq -r .updateId <<<"$mint_transaction"): user coins $coins_before -> $coins_after"
    else
      fail_check "C5 Mint committed, but user coins $coins_before -> $coins_after"
    fi
  else
    fail_check "C5 Mint rejected"
  fi
}

# amulet_units <validator> <party>
amulet_units() {
  local amounts
  amounts="$(amulet_holdings "$1" "$2" | jq -c 'map(.amount)')"
  if [[ "$amounts" == "[]" ]]; then echo 0; else json_decimal_array_total_units "$DAML_DECIMAL_FRACTION_DIGITS" "$amounts"; fi
}

# locked_ledger_balance_units: the LockedLedger balance of the gg deployment, read as guardianGovernance.
locked_ledger_balance_units() {
  local balance
  balance="$(active_contracts app-provider "$GG_PARTY" ":Wormhole.Ntt.Ledger:LockedLedger" \
    | jq -er --arg address "$lock_unlock_manager_address" '[.[] | select(.createArgument.managerAddress == $address)]
        | if length == 1 then .[0].createArgument.balance else error("expected one LockedLedger") end')"
  decimal_to_units "$balance" "$DAML_DECIMAL_FRACTION_DIGITS"
}

check_transfer_through_service() {
  local locker_user locker_party locker_inputs flow registry transaction balance_before balance_after
  echo "== C6. (c) Outbound Transfer (lock of Canton Coin) as a user on app-user with the service set"
  locker_user="t1-locker-$run_label"
  locker_party="$(wallet_onboard app-user "$locker_user")"
  wallet_tap app-user "$locker_user" "$TAP_AMOUNT_USD"
  locker_inputs="$(amulet_holdings app-user "$locker_party" | jq -c '[.[].contractId]')"
  flow="$(flow_set transfer "manager=$lock_unlock_manager_address")" || { fail_check "C6 transfer flow"; return; }
  echo "transfer set roles $(jq -c '[.disclosures[].role]' <<<"$flow") missing $(jq -c '[.missing[].role]' <<<"$flow")"
  check_flow_missing_roles "$flow" '["committed TransferFactory", "custodian TransferPreapproval"]' \
    || fail_check "C6 transfer set missing roles $(jq -c '[.missing[].role]' <<<"$flow")"
  # The Amulet registry (Scan) supplies the factory, its choice context and the custodian TransferPreapproval.
  registry="$(amulet_transfer_factory "$locker_party" "$NTT_CUSTODY_PARTY" "$LOCK_AMOUNT" "$locker_inputs")"
  [[ "$(jq -r .transferKind <<<"$registry")" == "direct" ]] || fail_check "C6 locker-to-nttCustody kind $(jq -r .transferKind <<<"$registry")"
  balance_before="$(locked_ledger_balance_units)"
  if transaction="$(submit_local app-user "t1-lock-$run_label" "$(jq -n --arg party "$locker_party" '[$party]')" \
      "$(exercise_command "$NTT_MANAGER_TEMPLATE" "$(flow_role_cid "$flow" NttManager)" Transfer "$(jq -n \
        --arg user "$locker_party" --arg emitter "$(flow_role_cid "$flow" "transceiver Emitter")" \
        --arg coreState "$(flow_role_cid "$flow" CoreState)" --arg ledger "$(flow_role_cid "$flow" LockedLedger)" \
        --arg chain "$(ntt_contract lockUnlockGg | jq -er .peerChainId)" --arg recipient "$PEER_RECIPIENT_ADDRESS" \
        --arg rawAmount "$LOCK_RAW_AMOUNT" --arg nonce "$OUTBOUND_NONCE" --argjson inputs "$locker_inputs" \
        --argjson extraArgs "$(registry_extra_args "$registry")" '{
          user: $user, transceiverEmitterCid: $emitter, coreStateCid: $coreState, ledgerCid: $ledger,
          recipientChain: $chain, recipientAddress: $recipient, rawAmount: $rawAmount, nonce: $nonce, consistencyLevel: "0",
          inputHoldingCids: $inputs, feeAllocation: null, extraArgs: $extraArgs}')")" \
      "$(jq -c '. + $registry' --argjson registry "$(registry_disclosures "$registry")" <<<"$(flow_disclosed_contracts "$flow")")")"; then
    balance_after="$(locked_ledger_balance_units)"
    if (( balance_after - balance_before == $(decimal_to_units "$LOCK_AMOUNT" "$DAML_DECIMAL_FRACTION_DIGITS") )); then
      pass "C6 Transfer update $(jq -r .updateId <<<"$transaction"): LockedLedger units $balance_before -> $balance_after, message $(message_summary "$(exercise_result "$transaction" :Wormhole.Ntt.Manager:NttManager Transfer)")"
    else
      fail_check "C6 Transfer committed, LockedLedger units $balance_before -> $balance_after"
    fi
  else
    fail_check "C6 Transfer rejected"
  fi
}

# release_vaa <sequence> <trimmed-amount>
release_vaa() {
  ntt_transfer_vaa -sequence "$1" -emitter-chain "$(ntt_contract lockUnlockGg | jq -er .peerChainId)" \
    -emitter-address "$(ntt_contract lockUnlockGg | jq -er .peerTransceiverAddress)" \
    -source-manager "$(ntt_contract lockUnlockGg | jq -er .peerManagerAddress)" -recipient-manager "$lock_unlock_manager_address" \
    -source-token "$LOCK_UNLOCK_PEER_TOKEN_ADDRESS" -recipient-party "$recipient_party" -ntt-message-id "$1" \
    -trimmed-decimals 8 -trimmed-amount "$2"
}

# submit_release_response <command-id> <vaa-json> <flow-json> <amount>: response body and HTTP status line.
submit_release_response() {
  local vaa="$2" flow="$3" registry
  registry="$(amulet_transfer_factory "$NTT_CUSTODY_PARTY" "$recipient_party" "$4" \
    "$(jq -n --arg pot "$(flow_role_cid "$flow" "custody pot Holding")" '[$pot]')")"
  submit_local_response app-user "$1" "$relayer_only" \
    "$(exercise_command "$NTT_MANAGER_TEMPLATE" "$(flow_role_cid "$flow" NttManager)" Release "$(jq -n \
      --arg relayer "$relayer_party" --arg coreState "$(flow_role_cid "$flow" CoreState)" \
      --arg replayNode "$(flow_role_cid "$flow" "covering ReplayNode")" --arg vaa "$(jq -r .vaaHex <<<"$vaa")" \
      --argjson keys "$(vaa_pub_keys_json "$vaa")" --arg recipient "$recipient_party" \
      --arg ledger "$(flow_role_cid "$flow" LockedLedger)" --argjson extraArgs "$(registry_extra_args "$registry")" '{
        executor: $relayer, coreStateCid: $coreState, replayNodeCid: $replayNode, vaaBytes: $vaa, pubKeys: $keys,
        recipient: $recipient, ledgerCid: $ledger, inputHoldingCids: [], extraArgs: $extraArgs}')")" \
    "$(jq -c '. + $registry' --argjson registry "$(registry_disclosures "$registry")" <<<"$(flow_disclosed_contracts "$flow")")"
}

# check_release <case> <command-id> <vaa-json> <flow-json> <amount>
check_release() {
  local response recipient_before recipient_after
  recipient_before="$(amulet_units app-user "$recipient_party")"
  response="$(submit_release_response "$2" "$3" "$4" "$5")"
  if [[ "$(tail -n 1 <<<"$response")" != "200" ]]; then
    fail_check "$1 Release rejected: $(sed '$d' <<<"$response" | head -c 400)"
    return
  fi
  recipient_after="$(amulet_units app-user "$recipient_party")"
  if (( recipient_after - recipient_before == $(decimal_to_units "$5" "$DAML_DECIMAL_FRACTION_DIGITS") )); then
    pass "$1 Release update $(sed '$d' <<<"$response" | jq -r .transaction.updateId): recipient units $recipient_before -> $recipient_after"
  else
    fail_check "$1 Release committed, recipient units $recipient_before -> $recipient_after"
  fi
}

check_release_through_service() {
  local recipient_user vaa flow
  echo "== C7. (b) Release on the Canton Coin deployment (reserve custodian nttCustody) with the service set"
  recipient_user="t1-recipient-$run_label"
  recipient_party="$(wallet_onboard app-user "$recipient_user")"
  wallet_tap app-user "$recipient_user" "$TAP_AMOUNT_USD"
  wallet_create_transfer_preapproval app-user "$recipient_user" > /dev/null
  vaa="$(release_vaa "$run_label" "$RELEASE_TRIMMED_AMOUNT")"
  flow="$(flow_set release "manager=$lock_unlock_manager_address&digest=$(jq -er .digestHex <<<"$vaa")&recipient=$recipient_party")" \
    || { fail_check "C7 release flow"; return; }
  echo "release set roles $(jq -c '[.disclosures[].role]' <<<"$flow") missing $(jq -c '[.missing[].role]' <<<"$flow")"
  check_flow_missing_roles "$flow" '["committed TransferFactory", "recipient TransferPreapproval"]' \
    || fail_check "C7 release set missing roles $(jq -c '[.missing[].role]' <<<"$flow")"
  pre_release_flow="$flow"
  check_release C7 "t1-release-$run_label" "$vaa" "$flow" "$RELEASE_AMOUNT"
}

check_stale_disclosure_fails() {
  local vaa response fresh_flow poll stale_ledger_cid fresh_ledger_cid
  echo "== C8. A stale set (LockedLedger and pot from before the release) fails; a fresh set succeeds"
  [[ -n "$pre_release_flow" ]] || { fail_check "C8 needs the C7 release set"; return; }
  stale_ledger_cid="$(flow_role_cid "$pre_release_flow" LockedLedger)" || { fail_check "C8 C7 set has no LockedLedger"; return; }
  vaa="$(release_vaa "$((run_label + 1))" "$SECOND_RELEASE_TRIMMED_AMOUNT")"
  response="$(submit_release_response "t1-release-stale-$run_label" "$vaa" "$pre_release_flow" "$SECOND_RELEASE_AMOUNT")"
  echo "stale response: HTTP $(tail -n 1 <<<"$response") $(sed '$d' <<<"$response" | jq -c '{code, cause: (.cause // "" | .[0:300])}' 2> /dev/null || sed '$d' <<<"$response" | head -c 300)"
  if [[ "$(tail -n 1 <<<"$response")" != "200" ]] \
      && [[ "$(sed '$d' <<<"$response" | jq -r '.code // empty' 2> /dev/null)" == "LOCAL_VERDICT_INACTIVE_CONTRACTS" ]]; then
    pass "C8 stale set (LockedLedger ${stale_ledger_cid:0:16} and pot from before the release): LOCAL_VERDICT_INACTIVE_CONTRACTS"
  else
    fail_check "C8 stale set: HTTP $(tail -n 1 <<<"$response")"
  fi
  # The service caches ledger reads for --cache-ttl (default 5 s): a relayer fetches again until the set changes.
  for (( poll = 1; poll <= FRESH_SET_MAX_POLLS; poll++ )); do
    fresh_flow="$(flow_set release "manager=$lock_unlock_manager_address&digest=$(jq -er .digestHex <<<"$vaa")&recipient=$recipient_party")" \
      || { fail_check "C8 fresh release flow"; return; }
    fresh_ledger_cid="$(flow_role_cid "$fresh_flow" LockedLedger)" || { fail_check "C8 fresh set has no LockedLedger"; return; }
    [[ "$fresh_ledger_cid" == "$stale_ledger_cid" ]] || break
    sleep 1
  done
  if [[ "$fresh_ledger_cid" == "$stale_ledger_cid" ]]; then
    fail_check "C8 the service still returns the archived LockedLedger after $FRESH_SET_MAX_POLLS fetches"
    return
  fi
  pass "C8 the service returns the new LockedLedger ${fresh_ledger_cid:0:16} at fetch $poll"
  check_release "C8 control" "t1-release-fresh-$run_label" "$vaa" "$fresh_flow" "$SECOND_RELEASE_AMOUNT"
}

# run_relayer_checks: C3 to C8. The relayer reads disclosure sets only from SERVICE_URL.
run_relayer_checks() {
  run_label="$(date +%s)"
  pre_release_flow=""
  lock_unlock_manager_address="$(ntt_contract lockUnlockGg | jq -er .managerAddress)"
  echo "service $SERVICE_URL"
  check_service_endpoints
  echo "== C4. The relayer on app-user is not a stakeholder"
  relayer_party="$(allocate_local_party app-user "T1Relayer-$run_label")"
  relayer_only="$(jq -n --arg relayer "$relayer_party" '[$relayer]')"
  echo "relayer $relayer_party"
  check_relayer_is_not_stakeholder
  check_mint_through_service
  check_transfer_through_service
  check_release_through_service
  check_stale_disclosure_fails
}

# report_check_summary <subcommand>: exits non-zero if one check failed.
report_check_summary() {
  echo "gateway_t1.sh $1: $check_passes passed, $check_failures failed"
  if (( check_failures > 0 )); then
    echo "gateway_t1.sh $1: FAIL" >&2
    exit 1
  fi
  echo "gateway_t1.sh $1: PASS"
}

gateway_check() {
  container_exists "$SERVICE_CONTAINER" || fail "$SERVICE_CONTAINER does not run; run gateway_t1.sh up"
  check_proxy_requires_client_certificate
  check_gateway_network_isolation
  run_relayer_checks
  report_check_summary check
}

case "${1:-}" in
  up) gateway_up ;;
  check) gateway_check ;;
  down) gateway_down ;;
  ledger-user) ensure_gateway_ledger_user ;;
  check-relayer) run_relayer_checks; report_check_summary check-relayer ;;
  *) echo "usage: gateway_t1.sh up|check|down|ledger-user|check-relayer" >&2; exit 2 ;;
esac
