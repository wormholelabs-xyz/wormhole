#!/usr/bin/env bash
# The console scripts below each hold one copy of the owner signer block (console --bootstrap takes one file).
# This test fails when the copies differ. Needs no Docker.
# Usage: canton/localnet/test/owner_signer_block_test.sh
set -euo pipefail
CANTON_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
readonly CANTON_DIR
readonly OWNER_SIGNER_BLOCK_FILES=(
  "$CANTON_DIR/devnet/guardian_governance.canton"
  "$CANTON_DIR/localnet/console/gg_rotate_owner_key.canton"
  "$CANTON_DIR/localnet/console/custody_allocate_party.canton"
)
readonly BLOCK_BEGIN_LINE="// BEGIN owner signer block"
readonly BLOCK_END_LINE="// END owner signer block"

# owner_signer_block <file>: the lines from BLOCK_BEGIN_LINE to BLOCK_END_LINE. Fails unless each marker occurs once.
owner_signer_block() {
  local begin_count end_count
  begin_count="$(grep -c "^$BLOCK_BEGIN_LINE" "$1" || true)"
  end_count="$(grep -cx "$BLOCK_END_LINE" "$1" || true)"
  if [[ "$begin_count" != 1 || "$end_count" != 1 ]]; then
    echo "FAIL: $1 has $begin_count begin and $end_count end markers, expected 1 each" >&2
    return 1
  fi
  sed -n "\\|^$BLOCK_BEGIN_LINE|,\\|^$BLOCK_END_LINE\$|p" "$1"
}

reference_block="$(owner_signer_block "${OWNER_SIGNER_BLOCK_FILES[0]}")"
if [[ "$(wc -l <<<"$reference_block" | tr -d ' ')" -lt 5 ]]; then
  echo "FAIL: the owner signer block of ${OWNER_SIGNER_BLOCK_FILES[0]} is shorter than 5 lines" >&2
  exit 1
fi
for console_script in "${OWNER_SIGNER_BLOCK_FILES[@]:1}"; do
  if ! diff <(printf '%s\n' "$reference_block") <(owner_signer_block "$console_script"); then
    echo "FAIL: the owner signer block of $console_script differs from ${OWNER_SIGNER_BLOCK_FILES[0]}" >&2
    exit 1
  fi
done
echo "PASS: ${#OWNER_SIGNER_BLOCK_FILES[@]} copies of the owner signer block are identical"
