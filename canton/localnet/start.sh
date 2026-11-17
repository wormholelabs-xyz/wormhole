#!/usr/bin/env bash
# Start Splice LocalNet (one SV and two validators) and wait until every Ledger API is ready.
set -euo pipefail
# shellcheck source=env.sh
source "$(dirname "${BASH_SOURCE[0]}")/env.sh"
# shellcheck source=image_lock_lib.sh
source "$(dirname "${BASH_SOURCE[0]}")/image_lock_lib.sh"

readonly READY_TIMEOUT_SECONDS=900

ensure_splice_checkout() {
  if [[ ! -d "$SPLICE_CHECKOUT/.git" ]]; then
    mkdir -p "$VENDOR_DIR"
    git clone --quiet --depth 1 --branch "$SPLICE_VERSION" --filter=blob:none --sparse \
      "$SPLICE_GIT_URL" "$SPLICE_CHECKOUT"
    git -C "$SPLICE_CHECKOUT" sparse-checkout set cluster/compose/localnet
  fi
  local checkout_commit
  checkout_commit="$(git -C "$SPLICE_CHECKOUT" rev-parse HEAD)"
  if [[ "$checkout_commit" != "$SPLICE_GIT_COMMIT" ]]; then
    echo "Splice checkout is at $checkout_commit, expected $SPLICE_GIT_COMMIT" >&2
    exit 1
  fi
  if [[ -n "$(git -C "$SPLICE_CHECKOUT" status --porcelain)" ]]; then
    echo "Splice checkout $SPLICE_CHECKOUT has local changes" >&2
    exit 1
  fi
}

wait_for_json_api() {
  local validator_name="$1"
  local readyz_url
  readyz_url="$(json_api_url "$validator_name")/readyz"
  local deadline=$((SECONDS + READY_TIMEOUT_SECONDS))
  until curl -sf -o /dev/null "$readyz_url"; do
    if ((SECONDS >= deadline)); then
      echo "Timeout: $readyz_url not ready after ${READY_TIMEOUT_SECONDS}s" >&2
      exit 1
    fi
    sleep 5
  done
  echo "$validator_name JSON Ledger API ready at $(json_api_url "$validator_name")"
}

ensure_splice_checkout
resolved_images_file="$(mktemp)"
trap 'rm -f "$resolved_images_file"' EXIT
"${LOCALNET_COMPOSE[@]}" config --images | sort -u > "$resolved_images_file"
if ! check_compose_images_are_locked "$resolved_images_file" "$LOCALNET_SCRIPT_DIR/images.lock"; then
  echo "Compose resolves images that images.lock does not list. Do not start LocalNet." >&2
  exit 1
fi
"${LOCALNET_COMPOSE[@]}" up -d --wait --wait-timeout "$READY_TIMEOUT_SECONDS"
for validator_name in sv app-provider app-user; do
  wait_for_json_api "$validator_name"
done
