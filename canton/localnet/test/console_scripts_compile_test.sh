#!/usr/bin/env bash
# Each guardian-namespace console script compiles with the Canton console of CANTON_RELEASE.
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
  "rotation${CASE_SEPARATOR}$PRELUDE $CONSOLE_DIR/gg_rotate_owner_key.canton${CASE_SEPARATOR}missing required environment variable: GG_PARTY_ID"
  "allocation${CASE_SEPARATOR}$PRELUDE $CONSOLE_DIR/custody_allocate_party.canton${CASE_SEPARATOR}missing required environment variable: GG_PARTY_ID"
  "show-topology${CASE_SEPARATOR}$CONSOLE_DIR/gg_show_topology.canton${CASE_SEPARATOR}failed with an exception (GG_PARTY_ID)"
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
