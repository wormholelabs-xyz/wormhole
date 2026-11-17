#!/usr/bin/env bash
# Run the Wormhole NTT layer end to end on LocalNet:
#   1. Upload and vet the NTT DARs on all three participants.
#   2. Create the NttGovernance root and the canonical CoinFactory (guardianGovernance ceremony).
#   3. Start the guardian watcher harness on app-provider as guardianGovernance.
#   4. Burn/mint deployment: register, set the peer, relay a Mint, burn.
#   5. Lock/unlock deployment of Canton Coin: register, set the peer, lock, relay a Release.
#   6. Stop the watcher and check the live observations. Reobserve the two outbound transfers.
# Usage: ntt_e2e.sh
# Precondition: LocalNet runs, gg_host.sh and gg_genesis.sh ran, and the NTT DARs are built.
set -euo pipefail
readonly SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

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
