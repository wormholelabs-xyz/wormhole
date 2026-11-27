#!/usr/bin/env bash
#
# Start a Canton sandbox serving the Ledger API v2 (gRPC) on :6865. Used by the
# Tilt `canton` component. Canton's `sandbox` command binds the Ledger API on
# 6865 by default and does NOT take the legacy `daml`-assistant `--dar`/`--port`
# flags. The DAR is uploaded+vetted later by bootstrap.sh (`dpm script
# --upload-dar`), not preloaded here.
#
# IMPORTANT: run from a directory with no `*.canton` file or daml.yaml — `dpm
# sandbox` auto-loads a bootstrap/init-script from its working dir otherwise.
set -euo pipefail

cd /tmp

# Bind the Ledger API to 0.0.0.0 so the guardian pod can reach it across the k8s
# service (the default bind is 127.0.0.1). NOTE: confirm the participant node
# name ("sandbox") and config key against `dpm sandbox --help` for the pinned SDK.
echo "[canton] starting dpm sandbox (Ledger API gRPC on :6865)"
exec dpm sandbox --no-tty \
  -C canton.participants.sandbox.ledger-api.address=0.0.0.0
