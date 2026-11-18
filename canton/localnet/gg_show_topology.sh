#!/usr/bin/env bash
# Print the guardianGovernance namespace and hosting as each LocalNet participant sees them.
# Usage: gg_show_topology.sh
set -euo pipefail
# shellcheck source=env.sh
source "$(dirname "${BASH_SOURCE[0]}")/env.sh"

gg_party_id="$(jq -er .partyId "$LOCALNET_STATE_DIR/gg-artifacts.json")"
GG_PARTY_ID="$gg_party_id" \
  "$LOCALNET_SCRIPT_DIR/canton_console.sh" "$LOCALNET_SCRIPT_DIR/console/gg_show_topology.canton" 2>&1 \
  | grep -E '^TOPOLOGY|ERROR|error'
