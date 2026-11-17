# Shared settings for the LocalNet scripts. Source this file; do not run it.
# shellcheck shell=bash disable=SC2034

LOCALNET_SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$LOCALNET_SCRIPT_DIR/../.." && pwd)"

# Splice release that DevNet and MainNet run (Canton 3.7.x).
SPLICE_VERSION="0.10.2"
SPLICE_GIT_URL="https://github.com/hyperledger-labs/splice"
SPLICE_GIT_COMMIT="e214318836feb7cee420dee33e85c7a6abbc7c04"
# Third-party checkout lives outside every repo.
VENDOR_DIR="${CANTON_VENDOR_DIR:-$(cd "$REPO_ROOT/.." && pwd)/vendor}"
SPLICE_CHECKOUT="$VENDOR_DIR/splice-$SPLICE_VERSION"

export LOCALNET_DIR="$SPLICE_CHECKOUT/cluster/compose/localnet"
export IMAGE_TAG="$SPLICE_VERSION"
# Host port 5432 is often taken by a local PostgreSQL.
export DB_PORT="${DB_PORT:-5440}"
# Stable protocol version only; set to env/alpha-protocol-version.env for alpha PVs.
export ALPHA_PROTOCOL_VERSION_ENV="${ALPHA_PROTOCOL_VERSION_ENV:-}"

COMPOSE_PROJECT="canton-localnet"

# Ledger API user that Daml Script runs as (see create_script_user.sh).
SCRIPT_USER_ID="wormhole-script"
LOCALNET_COMPOSE=(
  docker compose
  --project-name "$COMPOSE_PROJECT"
  --env-file "$LOCALNET_DIR/compose.env"
  --env-file "$LOCALNET_DIR/env/common.env"
  -f "$LOCALNET_DIR/compose.yaml"
  -f "$LOCALNET_DIR/resource-constraints.yaml"
  -f "$LOCALNET_SCRIPT_DIR/compose.digests.yaml"
  --profile sv
  --profile app-provider
  --profile app-user
)

# Validator name -> host port prefix (Ledger API <prefix>901, JSON API <prefix>975).
validator_port_prefix() {
  case "$1" in
    app-user) echo 2 ;;
    app-provider) echo 3 ;;
    sv) echo 4 ;;
    *) echo "unknown validator: $1 (use app-user, app-provider or sv)" >&2; return 1 ;;
  esac
}

json_api_url() {
  local port_prefix
  port_prefix="$(validator_port_prefix "$1")"
  echo "http://localhost:${port_prefix}975"
}

grpc_ledger_api_port() {
  local port_prefix
  port_prefix="$(validator_port_prefix "$1")"
  echo "${port_prefix}901"
}

# Keys, artifacts and contract ids that the guardianGovernance rehearsal scripts share.
LOCALNET_STATE_DIR="${LOCALNET_STATE_DIR:-$(cd "$REPO_ROOT/.." && pwd)/localnet-state}"

# Canton console of the same release as the LocalNet `canton` image (copied out of the container).
CANTON_RELEASE="3.7.3"
CANTON_CONSOLE_JAR="$VENDOR_DIR/canton-$CANTON_RELEASE/canton-open-source-$CANTON_RELEASE.jar"
CANTON_CONSOLE_CONFIG="$LOCALNET_SCRIPT_DIR/console/remote-participants.conf"
