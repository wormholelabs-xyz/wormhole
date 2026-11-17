#!/usr/bin/env bash
# G3 rehearsal: rotate the Wormhole guardian set on Canton from set N to set N+1.
#   1. SubmitGovernanceVAA with a GuardianSetUpgrade signed by the current set.
#   2. ParseAndVerifyVAA with a VAA signed by the new set. Expect success.
#   3. ParseAndVerifyVAA with a VAA signed by the old set. Expect success until the old set expires.
#   4. SubmitGovernanceVAA signed by the old set. Expect rejection (current set only).
#   5. ParseAndVerifyVAA naming the new set but signed by an old key. Expect rejection.
# Usage: gg_guardian_set_rotation.sh <old-set-key-indices> <new-set-key-indices>
#   Key indices are deterministic devnet keys (node/pkg/devnet), for example: 0 1
# Precondition: gg_genesis.sh ran. The old set is the current CoreState set.
# LocalNet runs on wall-clock time, so it cannot reach the 86400 s expiry. The Daml Script test
# Test.TestCore:testGuardianSetUpgradeExpiry covers the expiry with passTime.
set -euo pipefail
# shellcheck source=gg_lib.sh
source "$(dirname "${BASH_SOURCE[0]}")/gg_lib.sh"

old_key_indices="${1:?usage: gg_guardian_set_rotation.sh <old-set-key-indices> <new-set-key-indices>}"
new_key_indices="${2:?usage: gg_guardian_set_rotation.sh <old-set-key-indices> <new-set-key-indices>}"

current_core_state() {
  active_contracts "$SUBMITTING_VALIDATOR" "$OPERATOR_PARTY" :Wormhole.Core.State:CoreState \
    | jq -ec 'if length == 1 then .[0].createArgument else error("expected one CoreState") end'
}

old_set_index="$(current_core_state | jq -r .guardianSetIndex)"
new_set_index=$((old_set_index + 1))
run_sequence="$(date +%s)"
echo "Rotate guardian set $old_set_index (keys $old_key_indices) to $new_set_index (keys $new_key_indices)"

upgrade_vaa="$(generate_vaa -kind guardian-set-upgrade -signing-set-index "$old_set_index" \
  -signing-key-indices "$old_key_indices" -new-set-index "$new_set_index" -new-key-indices "$new_key_indices" \
  -sequence "$run_sequence")"
echo "Step 1: SubmitGovernanceVAA (GuardianSetUpgrade, digest $(jq -r .digestHex <<<"$upgrade_vaa"))"
upgrade_transaction="$(submit_governance_vaa "guardian-set-upgrade-$run_sequence" \
  "$(jq -r .vaaHex <<<"$upgrade_vaa")" "$(vaa_pub_keys_json "$upgrade_vaa")")"
record_core_contract_id coreState "$(created_contract_id "$upgrade_transaction" ":Wormhole.Core.State:CoreState")"
upgrade_record_time="$(jq -r .recordTime <<<"$upgrade_transaction")"
upgraded_state="$(current_core_state)"
echo "  record time $upgrade_record_time"
echo "  guardianSetIndex $(jq -r .guardianSetIndex <<<"$upgraded_state")"
echo "  guardianSets $(jq -c .guardianSets <<<"$upgraded_state")"
if [[ "$(jq -r .guardianSetIndex <<<"$upgraded_state")" != "$new_set_index" ]]; then
  echo "FAIL: guardianSetIndex is not $new_set_index" >&2
  exit 1
fi
old_set_expiry="$(jq -er --arg index "$old_set_index" '.guardianSets[] | select(.[0] == $index) | .[1].expirationTime' <<<"$upgraded_state")"
echo "PASS: set $new_set_index is current; set $old_set_index expires at $old_set_expiry"

expect_verified() {
  local label="$1" vaa_json="$2" expected_set_index="$3" verify_transaction verified_set_index
  verify_transaction="$(verify_vaa_as_operator "$label-$run_sequence" "$(jq -r .vaaHex <<<"$vaa_json")" "$(vaa_pub_keys_json "$vaa_json")")"
  verified_set_index="$(jq -r '[.events[] | .ExercisedEvent? // empty | select(.choice == "ParseAndVerifyVAA") | .exerciseResult.guardianSetIndex][0]' <<<"$verify_transaction")"
  if [[ "$verified_set_index" != "$expected_set_index" ]]; then
    echo "FAIL: $label verified with guardianSetIndex $verified_set_index, expected $expected_set_index" >&2
    exit 1
  fi
  echo "PASS: $label verified (guardianSetIndex $verified_set_index)"
}

expect_rejected() {
  local label="$1"
  shift
  if "$@" > "$gg_work_dir/$label.out" 2> "$gg_work_dir/$label.err"; then
    echo "FAIL: $label committed" >&2
    exit 1
  fi
  echo "PASS: $label rejected: $(grep -oE '"cause":"[^"]*"' "$gg_work_dir/$label.err" | head -n 1)"
}

echo "Step 2: VAA signed by the new set"
expect_verified new-set-message \
  "$(generate_vaa -kind message -signing-set-index "$new_set_index" -signing-key-indices "$new_key_indices" -sequence "$run_sequence")" \
  "$new_set_index"

echo "Step 3: VAA signed by the old set (valid until $old_set_expiry)"
expect_verified old-set-message \
  "$(generate_vaa -kind message -signing-set-index "$old_set_index" -signing-key-indices "$old_key_indices" -sequence "$run_sequence")" \
  "$old_set_index"

echo "Step 4: governance VAA signed by the old set"
old_set_governance="$(generate_vaa -kind set-message-fee -fee 0 -signing-set-index "$old_set_index" \
  -signing-key-indices "$old_key_indices" -sequence "$((run_sequence + 1))")"
expect_rejected old-set-governance submit_governance_vaa "old-set-governance-$run_sequence" \
  "$(jq -r .vaaHex <<<"$old_set_governance")" "$(vaa_pub_keys_json "$old_set_governance")"

echo "Step 5: VAA that names the new set but carries old-set signatures"
forged_new_set="$(generate_vaa -kind message -signing-set-index "$new_set_index" -signing-key-indices "$old_key_indices" -sequence "$run_sequence")"
expect_rejected new-set-index-old-keys verify_vaa_as_operator "new-set-index-old-keys-$run_sequence" \
  "$(jq -r .vaaHex <<<"$forged_new_set")" "$(vaa_pub_keys_json "$forged_new_set")"
