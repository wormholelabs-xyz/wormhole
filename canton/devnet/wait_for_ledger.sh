#!/usr/bin/env bash
#
# Block until the Canton Ledger API at $1:$2 is accepting TCP connections.
#
# Uses a raw TCP check rather than a CLI ledger query because dpm removed several
# `daml ledger ...` subcommands (replaced by the Declarative/JSON/gRPC APIs).
set -euo pipefail

HOST="${1:-localhost}"
PORT="${2:-6865}"

echo "[canton] waiting for Ledger API at ${HOST}:${PORT} ..."
for _ in $(seq 1 120); do
  if (exec 3<>"/dev/tcp/${HOST}/${PORT}") 2>/dev/null; then
    exec 3>&- 3<&-
    echo "[canton] Ledger API port is open"
    exit 0
  fi
  sleep 2
done

echo "[canton] timed out waiting for Ledger API at ${HOST}:${PORT}" >&2
exit 1
