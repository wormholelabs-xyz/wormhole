#!/usr/bin/env bash
# Checks of the AWS T1 layout. Runs on the operator laptop after aws_t1_up.sh all.
# Usage: aws_t1_check.sh
#   A1  A TCP connect scan of the participant public IP finds each port closed or filtered.
#       participant-sg admits traffic from gateway-sg only.
#   A2  The disclosure service answers /v1/healthz on the gateway public IP.
#   A3  From the gateway, only port 22 and the proxy port of the participant private IP accept a connection.
#   A4  The proxy refuses a client without a certificate and a client certificate of another CA.
#   A5  A relayer on the laptop completes Mint, a Canton Coin Transfer lock and a Release from the nttCustody reserve.
#       Disclosure sets come only from the gateway public IP. Commands go to app-user through an SSH tunnel.
#   A6  A stale disclosure set fails with LOCAL_VERDICT_INACTIVE_CONTRACTS.
# A5 and A6 run gateway_t1.sh check-relayer (C3 to C8) with LOCALNET_HOST_PORT_OFFSET and the LocalNet state copy.
# The app-provider tunnel serves only the oracle reads of the checks (LockedLedger balance).
# shellcheck disable=SC2154 # resource variables come from load_resources
set -euo pipefail
# shellcheck source=aws_t1_lib.sh
source "$(dirname "${BASH_SOURCE[0]}")/aws_t1_lib.sh"

readonly SCAN_PORTS=(22 "$PROXY_PORT" 2975 3975 4975 3901 5432)
readonly CONNECT_SECONDS=3
readonly TUNNEL_READY_MAX_POLLS=20
# LocalNet ports that the relayer checks reach through the participant tunnel.
readonly TUNNELED_LOCALNET_PORTS=(2975 2903 3975 4000)
readonly LOCAL_SCAN_PORT=$((4000 + LOCAL_TUNNEL_PORT_OFFSET))

load_resources
start_evidence_log aws_t1_check "aws_t1_check.sh" "$@"
readonly SERVICE_URL="http://$gateway_public_ip:$SERVICE_PORT"
echo "participant public $participant_public_ip private $participant_private_ip, gateway $gateway_public_ip"

check_failures=0
check_passes=0
pass() { echo "PASS: $*"; check_passes=$((check_passes + 1)); }
fail_check() { echo "FAIL: $*" >&2; check_failures=$((check_failures + 1)); }

tunnel_pids=()
work_dir="$(mktemp -d)"
cleanup() {
  local tunnel_pid
  for tunnel_pid in "${tunnel_pids[@]}"; do kill "$tunnel_pid" 2> /dev/null || true; done
  rm -rf "$work_dir"
}
trap cleanup EXIT

# await_local_port <port>
await_local_port() {
  local poll
  for (( poll = 1; poll <= TUNNEL_READY_MAX_POLLS; poll++ )); do
    nc -z -G 1 127.0.0.1 "$1" 2> /dev/null && return 0
    sleep 1
  done
  fail "local tunnel port $1 is not open"
}

# start_tunnel <host> <-L forwards...>
start_tunnel() {
  local host="$1"
  shift
  ssh_t1 -N "$@" "$host" &
  tunnel_pids+=("$!")
}

check_participant_public_ports() {
  local port ingress
  echo "== A1. Participant public IP $participant_public_ip from the laptop"
  for port in "${SCAN_PORTS[@]}"; do
    if nc -z -G "$CONNECT_SECONDS" -w "$CONNECT_SECONDS" "$participant_public_ip" "$port" 2> /dev/null; then
      fail_check "A1 $participant_public_ip:$port open"
    else
      pass "A1 $participant_public_ip:$port closed or filtered"
    fi
  done
  ingress="$(aws_ec2 describe-security-groups --group-ids "$participant_sg_id" | jq -c '.SecurityGroups[0].IpPermissions')"
  echo "participant-sg ingress $ingress"
  if jq -e --arg gateway "$gateway_sg_id" 'length > 0 and all(.[];
      (.IpRanges | length) == 0 and (.Ipv6Ranges | length) == 0 and (.PrefixListIds | length) == 0
      and (.UserIdGroupPairs | map(.GroupId)) == [$gateway])' <<<"$ingress" > /dev/null; then
    pass "A1 participant-sg ingress only from gateway-sg ($(jq -c 'map("\(.FromPort)")' <<<"$ingress"))"
  else
    fail_check "A1 participant-sg ingress $ingress"
  fi
}

check_gateway_healthz() {
  local status
  echo "== A2. Disclosure service on the gateway public IP"
  status="$(curl --connect-timeout 5 --max-time 15 -sS -o "$work_dir/healthz.json" -w '%{http_code}' "$SERVICE_URL/v1/healthz" || echo none)"
  if [[ "$status" == "200" ]] && jq -e '(.disclosingParties | length) == 2 and .ledgerEnd > 0' "$work_dir/healthz.json" > /dev/null; then
    pass "A2 $SERVICE_URL/v1/healthz: HTTP 200 $(jq -c '{disclosingParties, ledgerEnd}' "$work_dir/healthz.json")"
  else
    fail_check "A2 $SERVICE_URL/v1/healthz: HTTP $status $(head -c 300 "$work_dir/healthz.json" 2> /dev/null)"
  fi
}

check_gateway_reach() {
  local port probe_script states state
  echo "== A3. Participant private IP $participant_private_ip from the gateway"
  probe_script="for port in 22 $PROXY_PORT ${PARTICIPANT_CLOSED_PORTS[*]}; do
    if nc -z -w $CONNECT_SECONDS $participant_private_ip \$port 2> /dev/null; then echo \"\$port open\"; else echo \"\$port closed\"; fi
  done"
  states="$(ssh_t1 t1-gateway "$probe_script")"
  for port in 22 "$PROXY_PORT"; do
    state="$(awk -v port="$port" '$1 == port {print $2}' <<<"$states")"
    if [[ "$state" == "open" ]]; then pass "A3 gateway -> $participant_private_ip:$port open"; else fail_check "A3 gateway -> $participant_private_ip:$port $state"; fi
  done
  for port in "${PARTICIPANT_CLOSED_PORTS[@]}"; do
    state="$(awk -v port="$port" '$1 == port {print $2}' <<<"$states")"
    if [[ "$state" == "closed" ]]; then pass "A3 gateway -> $participant_private_ip:$port closed"; else fail_check "A3 gateway -> $participant_private_ip:$port $state"; fi
  done
}

# proxy_status <curl TLS flags...>: HTTP status of GET /v2/version through the proxy tunnel, or "tls-failure".
proxy_status() {
  curl --connect-timeout 5 --max-time 15 -s -o "$work_dir/proxy-body" -w '%{http_code}' \
    --resolve "$PROXY_SERVER_NAME:$LOCAL_PROXY_TUNNEL_PORT:127.0.0.1" --cacert "$CERT_DIR/ca.crt" "$@" \
    "https://$PROXY_SERVER_NAME:$LOCAL_PROXY_TUNNEL_PORT/v2/version" || echo "tls-failure"
}

check_proxy_requires_client_certificate() {
  local status
  echo "== A4. The proxy requires a client certificate of the T1 CA (TCP from the gateway through an SSH forward)"
  start_tunnel t1-gateway -L "$LOCAL_PROXY_TUNNEL_PORT:$participant_private_ip:$PROXY_PORT"
  await_local_port "$LOCAL_PROXY_TUNNEL_PORT"
  status="$(proxy_status --cert "$CERT_DIR/gateway-client.crt" --key "$CERT_DIR/gateway-client.key")"
  if [[ "$status" == "200" ]]; then pass "A4 control gateway client certificate: HTTP 200 $(head -c 120 "$work_dir/proxy-body")"; else fail_check "A4 gateway client certificate: $status"; fi
  status="$(proxy_status)"
  if [[ "$status" == "400" ]] && grep -qF "No required SSL certificate was sent" "$work_dir/proxy-body"; then
    pass "A4 no client certificate: HTTP 400 'No required SSL certificate was sent'"
  else
    fail_check "A4 no client certificate: $status $(head -c 200 "$work_dir/proxy-body" 2> /dev/null)"
  fi
  status="$(proxy_status --cert "$CERT_DIR/rogue-client.crt" --key "$CERT_DIR/rogue-client.key")"
  if [[ "$status" == "400" ]] && grep -qF "SSL certificate error" "$work_dir/proxy-body"; then
    pass "A4 client certificate of another CA: HTTP 400 'The SSL certificate error'"
  else
    fail_check "A4 client certificate of another CA: $status"
  fi
}

# relayer_check_line <pattern>: the one PASS or FAIL line of the relayer run that matches.
relayer_check_line() {
  grep -E "^(PASS|FAIL): $1" "$work_dir/relayer.log" | head -n 1
}

check_relayer_through_gateway() {
  local forwards=() port relayer_status line case_name pattern
  echo "== A5, A6. Relayer on the laptop: sets from $SERVICE_URL, commands to app-user through an SSH tunnel"
  for port in "${TUNNELED_LOCALNET_PORTS[@]}"; do
    forwards+=(-L "$((port + LOCAL_TUNNEL_PORT_OFFSET)):127.0.0.1:$port")
  done
  start_tunnel t1-participant "${forwards[@]}"
  for port in "${TUNNELED_LOCALNET_PORTS[@]}"; do await_local_port "$((port + LOCAL_TUNNEL_PORT_OFFSET))"; done
  relayer_status=0
  LOCALNET_HOST_PORT_OFFSET="$LOCAL_TUNNEL_PORT_OFFSET" SCAN_URL="http://localhost:$LOCAL_SCAN_PORT" \
    LOCALNET_STATE_DIR="$AWS_T1_STATE_DIR/localnet-state" GATEWAY_T1_SERVICE_URL="$SERVICE_URL" \
    NTT_PIN_VERSION=0.4.0 NTT_GUARDIAN_SET_INDEX=0 NTT_GUARDIAN_KEY_INDICES=0 \
    "$LOCALNET_SCRIPT_DIR/gateway_t1.sh" check-relayer > "$work_dir/relayer.log" 2>&1 || relayer_status=$?
  sed 's/^/  relayer| /' "$work_dir/relayer.log"
  for case_name in "A5 Mint|C5 Mint update" "A5 Transfer lock|C6 Transfer update" "A5 Release|C7 Release update" \
      "A6 stale set|C8 stale LockedLedger .*: LOCAL_VERDICT_INACTIVE_CONTRACTS"; do
    pattern="${case_name#*|}"
    line="$(relayer_check_line "$pattern")"
    if [[ "$line" == PASS:* ]]; then pass "${case_name%%|*}: ${line#PASS: }"; else fail_check "${case_name%%|*}: ${line:-no result}"; fi
  done
  if (( relayer_status == 0 )); then
    pass "A5 gateway_t1.sh check-relayer: $(grep -E '^gateway_t1.sh check-relayer: [0-9]+ passed' "$work_dir/relayer.log")"
  else
    fail_check "A5 gateway_t1.sh check-relayer exit status $relayer_status"
  fi
}

check_participant_public_ports
check_gateway_healthz
check_gateway_reach
check_proxy_requires_client_certificate
check_relayer_through_gateway
echo "aws_t1_check.sh: $check_passes passed, $check_failures failed"
if (( check_failures > 0 )); then
  echo "aws_t1_check.sh: FAIL" >&2
  exit 1
fi
echo "aws_t1_check.sh: PASS"
