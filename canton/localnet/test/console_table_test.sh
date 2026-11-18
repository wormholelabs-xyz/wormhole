#!/usr/bin/env bash
# Offline table tests of the guardian-namespace console code (console/test/*_test.canton).
# Each test runs after owner_signing_prelude.canton, owner_key_rotation_lib.canton, table_test_prelude.canton and
# topology_fixtures.canton.
# Each test prints one TABLE line.
# Needs the console jar (console_test_lib.sh). Needs no LocalNet.
# Usage: canton/localnet/test/console_table_test.sh [<table-name>...]
set -euo pipefail
# shellcheck source=SCRIPTDIR/console_test_lib.sh
source "$(dirname "${BASH_SOURCE[0]}")/console_test_lib.sh"
require_console_jar

readonly CONSOLE_DIR="$LOCALNET_SCRIPT_DIR/console"
# Row: <table name> <test file under console/test> <expected case count>
readonly CONSOLE_TABLES=(
  "owner-signer owner_signer_test.canton 16"
  "hosting-policy hosting_policy_test.canton 18"
  "owner-key-rotation owner_key_rotation_test.canton 36"
  "rotation-safety-stop rotation_safety_stop_test.canton 11"
)

work_dir="$(mktemp -d)"
trap 'rm -rf "$work_dir"' EXIT
mkdir -p "$work_dir/directory with space"
printf '#!/bin/sh\nprintf %%s %s\n' "$(printf 'ef%.0s' $(seq 1 64))" > "$work_dir/directory with space/signer"
chmod +x "$work_dir/directory with space/signer"

selected_tables=("$@")
failed_tables=0
run_tables=0
for table_row in "${CONSOLE_TABLES[@]}"; do
  read -r table_name test_file expected_case_count <<<"$table_row"
  if [[ ${#selected_tables[@]} -gt 0 && ! " ${selected_tables[*]} " =~ \ $table_name\  ]]; then
    continue
  fi
  run_tables=$((run_tables + 1))
  console_status=0
  run_offline_console "$work_dir/$table_name.out" \
    OWNER_SIGNER_STUB_IN_DIRECTORY_WITH_SPACE="$work_dir/directory with space/signer" -- \
    "$CONSOLE_DIR/owner_signing_prelude.canton" "$CONSOLE_DIR/owner_key_rotation_lib.canton" \
    "$CONSOLE_DIR/test/table_test_prelude.canton" \
    "$CONSOLE_DIR/test/topology_fixtures.canton" "$CONSOLE_DIR/test/$test_file" \
    || console_status=$?
  grep -E '^CASE ' "$work_dir/$table_name.out" || true
  expected_table_line="TABLE $table_name cases=$expected_case_count failed=0"
  if [[ "$console_status" -ne 0 ]] || ! grep -qxF "$expected_table_line" "$work_dir/$table_name.out" \
      || grep -qE '^CASE .* fail' "$work_dir/$table_name.out"; then
    echo "FAIL $table_name: console status $console_status, expected: $expected_table_line"
    grep -E '^TABLE |Compilation Failed|error|ERROR' "$work_dir/$table_name.out" | head -n 20
    failed_tables=$((failed_tables + 1))
  else
    echo "PASS $table_name: $expected_case_count cases"
  fi
done
if [[ "$run_tables" -eq 0 ]]; then
  echo "FAIL: no table matches: $*" >&2
  exit 2
fi
if [[ "$failed_tables" -ne 0 ]]; then
  echo "FAIL: $failed_tables of $run_tables console tables"
  exit 1
fi
echo "PASS: $run_tables console tables"
