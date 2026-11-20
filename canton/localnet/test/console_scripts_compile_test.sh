#!/usr/bin/env bash
# Each listed console script compiles with the Canton console of CANTON_RELEASE.
# Offline, each script stops at its first runtime check. The expected stop text proves that compilation passed.
# Needs the console jar (console_test_lib.sh). Needs no LocalNet.
# Usage: canton/localnet/test/console_scripts_compile_test.sh
set -euo pipefail
# shellcheck source=SCRIPTDIR/console_test_lib.sh
source "$(dirname "${BASH_SOURCE[0]}")/console_test_lib.sh"
require_console_jar

readonly CONSOLE_DIR="$LOCALNET_SCRIPT_DIR/console"
readonly PRELUDE="$CONSOLE_DIR/owner_signing_prelude.canton"
readonly CASE_SEPARATOR=$'\x1f'
# Row: <name> <scripts, space-separated> <expected runtime stop text>
compile_cases=(
  "rotation${CASE_SEPARATOR}$PRELUDE $CONSOLE_DIR/owner_key_rotation_lib.canton $CONSOLE_DIR/gg_rotate_owner_key.canton${CASE_SEPARATOR}missing required environment variable: GG_PARTY_ID"
  "allocation${CASE_SEPARATOR}$PRELUDE $CONSOLE_DIR/custody_allocate_party.canton${CASE_SEPARATOR}missing required environment variable: GG_PARTY_ID"
  "show-topology${CASE_SEPARATOR}$CONSOLE_DIR/gg_show_topology.canton${CASE_SEPARATOR}failed with an exception (GG_PARTY_ID)"
  "party-replication${CASE_SEPARATOR}$PRELUDE $CONSOLE_DIR/owner_key_rotation_lib.canton $CONSOLE_DIR/party_replication_lib.canton $CONSOLE_DIR/party_replication.canton${CASE_SEPARATOR}missing required environment variable: REPLICATION_PHASE"
  "forge-coin-contract${CASE_SEPARATOR}$PRELUDE $CONSOLE_DIR/forge_coin_contract.canton${CASE_SEPARATOR}missing required environment variable: FORGE_OUTPUT_FILE"
  "guardian4-connect${CASE_SEPARATOR}$CONSOLE_DIR/guardian4_connect.canton${CASE_SEPARATOR}failed with an exception (guardian4 not bound)"
  "hosting-topology${CASE_SEPARATOR}$CONSOLE_DIR/hosting_topology.canton${CASE_SEPARATOR}failed with an exception (HOSTING_PARTY_IDS)"
  "acs-commitments${CASE_SEPARATOR}$CONSOLE_DIR/acs_commitments.canton${CASE_SEPARATOR}failed with an exception (COMMITMENT_PARTICIPANTS)"
  "package-vetting-toggle${CASE_SEPARATOR}$CONSOLE_DIR/package_vetting_toggle.canton${CASE_SEPARATOR}failed with an exception (VETTING_PACKAGE_IDS)"
  "participant-hold-disconnected${CASE_SEPARATOR}$CONSOLE_DIR/participant_hold_disconnected.canton${CASE_SEPARATOR}failed with an exception (HOLD_PARTICIPANT)"
  "repair-contracts${CASE_SEPARATOR}$CONSOLE_DIR/repair_contracts.canton${CASE_SEPARATOR}failed with an exception (REPAIR_PARTICIPANT)"
  "devnet-guardian-governance${CASE_SEPARATOR}$LOCALNET_SCRIPT_DIR/../devnet/guardian_governance.canton${CASE_SEPARATOR}missing required environment variable: GG_OWNER_PUBKEY_1"
)

work_dir="$(mktemp -d)"
trap 'rm -rf "$work_dir"' EXIT
failed_cases=0
for compile_case in "${compile_cases[@]}"; do
  IFS="$CASE_SEPARATOR" read -r case_name console_scripts expected_stop <<<"$compile_case"
  read -r -a console_script_list <<<"$console_scripts"
  run_offline_console "$work_dir/$case_name.out" -- "${console_script_list[@]}" || true
  if grep -qF 'Compilation Failed' "$work_dir/$case_name.out"; then
    echo "FAIL $case_name: compilation failed"
    grep -F 'error' "$work_dir/$case_name.out" | head -n 20
    failed_cases=$((failed_cases + 1))
  elif ! grep -qF "$expected_stop" "$work_dir/$case_name.out"; then
    echo "FAIL $case_name: expected the runtime stop: $expected_stop"
    tail -n 5 "$work_dir/$case_name.out"
    failed_cases=$((failed_cases + 1))
  else
    echo "PASS $case_name"
  fi
done
if [[ "$failed_cases" -ne 0 ]]; then
  echo "FAIL: $failed_cases of ${#compile_cases[@]} console scripts"
  exit 1
fi
echo "PASS: ${#compile_cases[@]} console scripts compile"
