#!/usr/bin/env bash
# Run the NTT steps of README.md "NTT end-to-end run" in sequence.
# Usage: ntt_e2e.sh
# Set NTT_UPLOAD_DARS=false to skip step 1 when the participants already vet the NTT packages.
# Precondition: LocalNet runs, gg_host.sh and gg_genesis.sh ran, and the NTT DARs at the pins of ntt_lib.sh are built.
# The VAAs use guardian set 1 with devnet key 1: run gg_guardian_set_rotation.sh 0 1 first,
# or set NTT_GUARDIAN_SET_INDEX=0 NTT_GUARDIAN_KEY_INDICES=0 to use the genesis set.
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
if [[ "${NTT_UPLOAD_DARS:-true}" == true ]]; then
  "$SCRIPT_DIR/ntt_upload_dars.sh"
else
  echo "NTT_UPLOAD_DARS=false: the participants keep their vetted NTT packages"
fi
echo "### 2. NttGovernance root and CoinFactory of the genesis"
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
