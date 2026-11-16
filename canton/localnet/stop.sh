#!/usr/bin/env bash
# Stop LocalNet. Usage: stop.sh [--delete-data]
# Without --delete-data the PostgreSQL volume stays, and start.sh resumes the same ledger.
set -euo pipefail
# shellcheck source=env.sh
source "$(dirname "${BASH_SOURCE[0]}")/env.sh"

case "${1:-}" in
  "") "${LOCALNET_COMPOSE[@]}" down ;;
  --delete-data) "${LOCALNET_COMPOSE[@]}" down --volumes ;;
  *)
    echo "usage: $0 [--delete-data]" >&2
    exit 1
    ;;
esac
