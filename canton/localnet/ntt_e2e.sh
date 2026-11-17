#!/usr/bin/env bash
# Run the NTT steps of README.md "NTT end-to-end run" in sequence.
# Usage: ntt_e2e.sh
# Precondition: LocalNet runs, gg_host.sh and gg_genesis.sh ran, and the NTT DARs at the pins of ntt_lib.sh are built.
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
readonly SCRIPT_DIR

watcher_started=false
stop_watcher_on_exit() {
  if [[ "$watcher_started" == true ]]; then
    "$SCRIPT_DIR/ntt_watcher.sh" stop || true
  fi
}
trap stop_watcher_on_exit EXIT

echo "### 1. NTT DARs"
"$SCRIPT_DIR/ntt_upload_dars.sh"
echo "### 2. NttGovernance root and CoinFactory"
"$SCRIPT_DIR/ntt_governance.sh"
echo "### 3. Guardian watcher"
"$SCRIPT_DIR/ntt_watcher.sh" start
watcher_started=true
echo "### 4. Burn/mint deployment"
"$SCRIPT_DIR/ntt_burn_mint.sh"
echo "### 5. Lock/unlock deployment of Canton Coin"
"$SCRIPT_DIR/ntt_lock_unlock.sh"
echo "### 6. Watcher observations"
watcher_started=false
"$SCRIPT_DIR/ntt_watcher.sh" stop
"$SCRIPT_DIR/ntt_watcher.sh" check
"$SCRIPT_DIR/ntt_watcher.sh" reobserve
echo "NTT end-to-end run passed"
