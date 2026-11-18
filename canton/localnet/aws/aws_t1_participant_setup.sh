#!/usr/bin/env bash
# Runs on the AWS T1 participant instance. Starts a fresh LocalNet, hosts guardianGovernance and nttCustody, deploys
# NTT 0.4.0, makes the read-only ledger user of the disclosure service, then starts the mutual-TLS ledger proxy.
# Usage: aws_t1_participant_setup.sh <work-dir> <private-ip>
#   <work-dir> holds wormhole/ (this tree), custody-lab/ (the tree of custody_host.sh), ntt/ (the NTT DAR) and t1-certs/.
# Output: <work-dir>/localnet-state, the container t1-ledger-proxy on <private-ip>:7443.
set -euo pipefail

readonly WORK_DIR="${1:?usage: aws_t1_participant_setup.sh <work-dir> <private-ip>}"
readonly PRIVATE_IP="${2:?usage: aws_t1_participant_setup.sh <work-dir> <private-ip>}"
readonly WORMHOLE_TREE="$WORK_DIR/wormhole"
readonly CUSTODY_TREE="$WORK_DIR/custody-lab"
readonly CERT_DIR="$WORK_DIR/t1-certs"
readonly PROXY_CONTAINER="t1-ledger-proxy"
readonly PROXY_PORT=7443
readonly APP_PROVIDER_JSON_API_PORT=3975

[[ "$PRIVATE_IP" =~ ^10\.71\.1\.[0-9]{1,3}$ ]] || { echo "private IP is not in the T1 subnet: $PRIVATE_IP" >&2; exit 2; }
for required_path in "$WORMHOLE_TREE/canton/localnet/start.sh" "$CUSTODY_TREE/canton/localnet/custody_host.sh" \
    "$CERT_DIR/ca.crt" "$CERT_DIR/$PROXY_CONTAINER.crt" "$CERT_DIR/$PROXY_CONTAINER.key"; do
  [[ -f "$required_path" ]] || { echo "missing $required_path" >&2; exit 1; }
done

step() { echo "=== $(date -u +%H:%M:%SZ) $*"; }

# Warm the Go module cache: generate_vaa runs `go run` with GOPROXY=off. go.sum pins each module.
step "go module cache"
# GOTOOLCHAIN=auto: the go directive of node/go.mod selects the toolchain (checksum database verified).
(cd "$WORMHOLE_TREE/node" && GOTOOLCHAIN=auto GOFLAGS=-mod=readonly go build -o /dev/null ./hack/canton_localnet_vaa)
switched_goroot="$(cd "$WORMHOLE_TREE/node" && GOTOOLCHAIN=auto go env GOROOT)"
export PATH="$switched_goroot/bin:$PATH"
echo "go $(go version)"

step "LocalNet start"
"$WORMHOLE_TREE/canton/localnet/start.sh"
step "upload_dars.sh app-provider"
"$WORMHOLE_TREE/canton/localnet/upload_dars.sh" app-provider
step "gg_host.sh"
"$WORMHOLE_TREE/canton/localnet/gg_host.sh"
step "custody_host.sh owner1 owner2 owner3"
"$CUSTODY_TREE/canton/localnet/custody_host.sh" owner1 owner2 owner3

export NTT_PIN_VERSION=0.4.0 NTT_GUARDIAN_SET_INDEX=0 NTT_GUARDIAN_KEY_INDICES=0 NTT_REPO_DIR="$WORK_DIR/ntt"
for script_name in gg_genesis.sh ntt_upload_dars.sh ntt_governance.sh ntt_burn_mint.sh ntt_lock_unlock_gg.sh; do
  step "$script_name"
  "$WORMHOLE_TREE/canton/localnet/$script_name"
done
step "gateway_t1.sh ledger-user"
"$WORMHOLE_TREE/canton/localnet/gateway_t1.sh" ledger-user

step "mutual-TLS ledger proxy on $PRIVATE_IP:$PROXY_PORT"
nginx_image="$(awk '$1 == "nginx/nginx-unprivileged" {print $1 ":" $2 "@" $3}' "$WORMHOLE_TREE/canton/localnet/images.lock")"
[[ "$nginx_image" =~ @sha256:[0-9a-f]{64}$ ]] || { echo "no digest-pinned nginx image in images.lock" >&2; exit 1; }
cat > "$CERT_DIR/ledger-proxy.conf" <<EOF
server {
  listen $PRIVATE_IP:$PROXY_PORT ssl;
  ssl_protocols TLSv1.2 TLSv1.3;
  ssl_certificate /etc/t1/$PROXY_CONTAINER.crt;
  ssl_certificate_key /etc/t1/$PROXY_CONTAINER.key;
  ssl_client_certificate /etc/t1/ca.crt;
  ssl_verify_client on;
  ssl_verify_depth 1;
  client_max_body_size 1m;
  location / {
    proxy_pass http://127.0.0.1:$APP_PROVIDER_JSON_API_PORT;
    proxy_http_version 1.1;
    proxy_connect_timeout 5s;
    proxy_read_timeout 60s;
  }
}
EOF
# Readable by the nginx user (101).
chmod 644 "$CERT_DIR/ledger-proxy.conf" "$CERT_DIR"/*.crt "$CERT_DIR/$PROXY_CONTAINER.key"
docker rm -f "$PROXY_CONTAINER" > /dev/null 2>&1 || true
docker run -d --name "$PROXY_CONTAINER" --restart unless-stopped --network host \
  -v "$CERT_DIR/ledger-proxy.conf:/etc/nginx/conf.d/default.conf:ro" -v "$CERT_DIR:/etc/t1:ro" "$nginx_image" > /dev/null
sleep 2
docker ps --filter "name=$PROXY_CONTAINER" --format '{{.Names}} {{.Status}}'
echo "listening sockets:"
ss -ltnH | awk '{print $4}' | sort -u
step "participant setup done"
