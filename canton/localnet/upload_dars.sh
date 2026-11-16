#!/usr/bin/env bash
# Upload the Wormhole core DAR and the test DAR to one LocalNet validator and vet all packages.
# Usage: upload_dars.sh [validator]   (app-user, app-provider or sv; default app-provider)
set -euo pipefail
# shellcheck source=env.sh
source "$(dirname "${BASH_SOURCE[0]}")/env.sh"

readonly CANTON_PROJECT_DIR="${CANTON_PROJECT_DIR:-$REPO_ROOT/canton}"
readonly DAR_PATHS=(
  "$CANTON_PROJECT_DIR/core/.daml/dist/wormhole-core-0.6.0.dar"
  "$CANTON_PROJECT_DIR/test/.daml/dist/wormhole-core-test-0.1.0.dar"
)

validator_name="${1:-app-provider}"
ledger_json_url="$(json_api_url "$validator_name")"
access_token="$("$LOCALNET_SCRIPT_DIR/token.sh")"

for dar_path in "${DAR_PATHS[@]}"; do
  if [[ ! -f "$dar_path" ]]; then
    echo "DAR not found: $dar_path (build it with 'dpm build --all' in $CANTON_PROJECT_DIR)" >&2
    exit 1
  fi
done

for dar_path in "${DAR_PATHS[@]}"; do
  echo "Upload $(basename "$dar_path") to $validator_name ($ledger_json_url)"
  http_status="$(curl -sS -o /dev/stderr -w '%{http_code}' \
    -X POST "$ledger_json_url/v2/dars?vetAllPackages=true" \
    -H "Authorization: Bearer $access_token" \
    -H 'Content-Type: application/octet-stream' \
    --data-binary "@$dar_path")"
  echo
  if [[ "$http_status" != "200" ]]; then
    echo "Upload of $dar_path failed with HTTP $http_status" >&2
    exit 1
  fi
done
