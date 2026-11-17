#!/usr/bin/env bash
# Start or remove the temporary guardian4 participant: a separate container on the LocalNet network, same Canton image,
# with its own PostgreSQL container (tmpfs storage).
# Usage: guardian4.sh <start|remove>
# Ports on 127.0.0.1: Ledger API 5901, Admin API 5902, JSON Ledger API 5975.
set -euo pipefail
# shellcheck source=env.sh
source "$(dirname "${BASH_SOURCE[0]}")/env.sh"

readonly GUARDIAN4_CONTAINER="guardian4"
readonly GUARDIAN4_DATABASE_CONTAINER="guardian4-postgres"
readonly LOCALNET_NETWORK="localnet"
readonly CANTON_IMAGE_NAME="ghcr.io/digital-asset/decentralized-canton-sync/docker/canton"
readonly POSTGRES_IMAGE_NAME="postgres"
readonly POSTGRES_IMAGE_TAG="18.6-trixie"
readonly READY_MAX_ATTEMPTS=90

# locked_image_reference <image name> <tag>: <name>:<tag>@<digest> from images.lock.
locked_image_reference() {
  local image_digest
  image_digest="$(awk -v name="$1" -v tag="$2" '$1 == name && $2 == tag {print $3}' "$LOCALNET_SCRIPT_DIR/images.lock")"
  if [[ ! "$image_digest" =~ ^sha256:[0-9a-f]{64}$ ]]; then
    echo "no digest for $1:$2 in images.lock" >&2
    return 1
  fi
  echo "$1:$2@$image_digest"
}

start_guardian4() {
  local attempt
  if docker container inspect "$GUARDIAN4_CONTAINER" >/dev/null 2>&1 || docker container inspect "$GUARDIAN4_DATABASE_CONTAINER" >/dev/null 2>&1; then
    echo "a guardian4 container exists; run guardian4.sh remove first" >&2
    return 1
  fi
  docker run -d --name "$GUARDIAN4_DATABASE_CONTAINER" --network "$LOCALNET_NETWORK" --tmpfs /var/lib/postgresql \
    -e POSTGRES_USER=guardian4 -e POSTGRES_PASSWORD=guardian4 -e POSTGRES_DB=guardian4 \
    "$(locked_image_reference "$POSTGRES_IMAGE_NAME" "$POSTGRES_IMAGE_TAG")" >/dev/null
  for ((attempt = 1; attempt <= READY_MAX_ATTEMPTS; attempt++)); do
    if docker exec "$GUARDIAN4_DATABASE_CONTAINER" pg_isready -U guardian4 -d guardian4 >/dev/null 2>&1; then
      break
    fi
    sleep 1
  done
  docker run -d --name "$GUARDIAN4_CONTAINER" --network "$LOCALNET_NETWORK" \
    -e "_JAVA_OPTIONS=-Xms512m -Xmx2g" \
    -p "$HOST_BIND_IP:5901:5901" -p "$HOST_BIND_IP:5902:5902" -p "$HOST_BIND_IP:5975:5975" \
    -v "$LOCALNET_SCRIPT_DIR/guardian4/guardian4.conf:/app/guardian4.conf:ro" \
    --entrypoint /app/bin/canton "$(locked_image_reference "$CANTON_IMAGE_NAME" "$SPLICE_VERSION")" \
    daemon --no-tty --log-level-stdout=INFO --log-file-appender=off -c /app/guardian4.conf >/dev/null
  for ((attempt = 1; attempt <= READY_MAX_ATTEMPTS; attempt++)); do
    if curl -s --connect-timeout 2 --max-time 5 -o /dev/null "http://127.0.0.1:5975/livez"; then
      echo "guardian4 is up"
      return 0
    fi
    sleep 2
  done
  echo "guardian4 did not start; see docker logs $GUARDIAN4_CONTAINER" >&2
  return 1
}

case "${1:-}" in
  start) start_guardian4 ;;
  remove) docker rm -f "$GUARDIAN4_CONTAINER" "$GUARDIAN4_DATABASE_CONTAINER" ;;
  *) echo "usage: guardian4.sh <start|remove>" >&2; exit 2 ;;
esac
