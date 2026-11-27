#!/usr/bin/env bash
# G3 rehearsal: rotate the Wormhole guardian set on Canton from set N to set N+1.
# GovernanceRelayer submits the governance VAAs; CoreVerifier submits the VAA checks (local parties on sv).
#   1. SubmitGovernanceVAA with a GuardianSetUpgrade signed by the current set, then
#      AttachGuardianPublicKeys with the keys that the upgrade does not carry over.
#   2. ParseAndVerifyVAA with a VAA signed by the new set. Expect success.
#   3. ParseAndVerifyVAA with a VAA signed by the old set. Expect success until the old set expires.
#   4. SubmitGovernanceVAA signed by the old set. Expect rejection (current set only).
#   5. ParseAndVerifyVAA naming the new set, with an old key at guardian index 0. Expect rejection.
#   6. New set of 2 or more guardians only: ParseAndVerifyVAA signed by quorum - 1 new guardians.
#      Expect "no quorum".
# Usage: gg_guardian_set_rotation.sh <old-set-key-indices> <new-set-key-indices>
#   Key indices are comma-separated deterministic devnet keys (node/pkg/devnet) in guardian order.
#   One-guardian sets: 0 1. Three-guardian new set (quorum 3): 1 2,3,4
# Precondition: gg_genesis.sh ran. The old set is the current CoreState set.
# LocalNet runs on wall-clock time, so it cannot reach the 86400 s expiry. The Daml Script test
# Test.TestCore:testGuardianSetUpgradeExpiry covers the expiry with passTime.
set -euo pipefail
# shellcheck source=gg_lib.sh
source "$(dirname "${BASH_SOURCE[0]}")/gg_lib.sh"

old_key_indices="${1:?usage: gg_guardian_set_rotation.sh <old-set-key-indices> <new-set-key-indices>}"
new_key_indices="${2:?usage: gg_guardian_set_rotation.sh <old-set-key-indices> <new-set-key-indices>}"
for key_indices in "$old_key_indices" "$new_key_indices"; do
  if [[ ! "$key_indices" =~ ^[0-9]+(,[0-9]+)*$ ]]; then
    echo "key indices must be comma-separated integers: $key_indices" >&2
    exit 2
  fi
done
IFS=, read -ra new_keys <<<"$new_key_indices"
first_old_key="${old_key_indices%%,*}"
for new_key in "${new_keys[@]}"; do
  if [[ "$first_old_key" == "$new_key" ]]; then
    echo "the first old key must not be a new key (step 5 signs with it at index 0)" >&2
    exit 2
  fi
done
new_set_size="${#new_keys[@]}"
# Wormhole quorum: floor(2n/3) + 1 (sdk/vaa CalculateQuorum).
new_set_quorum=$((new_set_size * 2 / 3 + 1))
# Step 5 signer list: the first old key in place of new guardian 0, then the other new keys.
forged_key_indices="$first_old_key"
for new_key in "${new_keys[@]:1}"; do
  forged_key_indices+=",$new_key"
done

current_core_state() {
  active_contracts "$SUBMITTING_VALIDATOR" "$GG_PARTY" :Wormhole.Core.State:CoreState \
    | jq -ec 'if length == 1 then .[0].createArgument else error("expected one CoreState") end'
}

old_set_index="$(current_core_state | jq -er .guardianSetIndex)"
new_set_index=$((old_set_index + 1))
run_sequence="$(date +%s)"
echo "Rotate guardian set $old_set_index (keys $old_key_indices) to $new_set_index (keys $new_key_indices)"

upgrade_vaa="$(generate_vaa -kind guardian-set-upgrade -signing-set-index "$old_set_index" \
  -signing-key-indices "$old_key_indices" -new-set-index "$new_set_index" -new-key-indices "$new_key_indices" \
  -sequence "$run_sequence")"
echo "Step 1: SubmitGovernanceVAA (GuardianSetUpgrade, digest $(jq -er .digestHex <<<"$upgrade_vaa"))"
upgrade_transaction="$(relay_governance_vaa "guardian-set-upgrade-$run_sequence" "$(jq -er .vaaHex <<<"$upgrade_vaa")")"
created_contract_id "$upgrade_transaction" ":Wormhole.Core.State:CoreState" >/dev/null
upgrade_record_time="$(jq -er .recordTime <<<"$upgrade_transaction")"
unkeyed_public_keys="$(jq -c --arg index "$new_set_index" --argjson upgradeVaa "$upgrade_vaa" \
  '[(.guardianSets[] | select(.[0] == $index) | .[1].members) as $members
    | range(0; $members | length) | select($members[.].publicKey == null) | $upgradeVaa.newGuardianPublicKeys[.]]' \
  <<<"$(current_core_state)")"
echo "Step 1b: AttachGuardianPublicKeys ($(jq length <<<"$unkeyed_public_keys") keys)"
if [[ "$(jq length <<<"$unkeyed_public_keys")" != 0 ]]; then
  attach_transaction="$(attach_guardian_public_keys "attach-guardian-public-keys-$run_sequence" "$new_set_index" "$unkeyed_public_keys")"
  created_contract_id "$attach_transaction" ":Wormhole.Core.State:CoreState" >/dev/null
fi
upgraded_state="$(current_core_state)"
if [[ "$(jq -r --arg index "$new_set_index" \
    '[.guardianSets[] | select(.[0] == $index) | .[1].members[] | select(.publicKey == null)] | length' <<<"$upgraded_state")" != 0 ]]; then
  echo "FAIL: set $new_set_index has a member without a public key" >&2
  exit 1
fi
echo "  record time $upgrade_record_time"
echo "  guardianSetIndex $(jq -er .guardianSetIndex <<<"$upgraded_state")"
echo "  guardianSets $(jq -c .guardianSets <<<"$upgraded_state")"
if [[ "$(jq -er .guardianSetIndex <<<"$upgraded_state")" != "$new_set_index" ]]; then
  echo "FAIL: guardianSetIndex is not $new_set_index" >&2
  exit 1
fi
old_set_expiry="$(jq -er --arg index "$old_set_index" '.guardianSets[] | select(.[0] == $index) | .[1].expirationTime' <<<"$upgraded_state")"
echo "PASS: set $new_set_index is current; set $old_set_index expires at $old_set_expiry"

expect_verified() {
  local label="$1" vaa_json="$2" expected_set_index="$3" verify_transaction verified_set_index
  verify_transaction="$(verify_vaa "$label-$run_sequence" "$(jq -er .vaaHex <<<"$vaa_json")")"
  verified_set_index="$(jq -r '[.events[] | .ExercisedEvent? // empty | select(.choice == "ParseAndVerifyVAA") | .exerciseResult.guardianSetIndex][0]' <<<"$verify_transaction")"
  if [[ "$verified_set_index" != "$expected_set_index" ]]; then
    echo "FAIL: $label verified with guardianSetIndex $verified_set_index, expected $expected_set_index" >&2
    exit 1
  fi
  echo "PASS: $label verified (guardianSetIndex $verified_set_index)"
}

# expect_rejected <label> <expected-assertion-message> <command...>: the command must fail with
# DAML_FAILURE and the Daml AssertionFailed message.
expect_rejected() {
  local label="$1" expected_message="$2"
  shift 2
  if "$@" > "$gg_work_dir/$label.out" 2> "$gg_work_dir/$label.err"; then
    echo "FAIL: $label committed" >&2
    exit 1
  fi
  if ! grep -qF 'DAML_FAILURE' "$gg_work_dir/$label.err" \
      || ! grep -qF "AssertionFailed (error category 9): $expected_message" "$gg_work_dir/$label.err"; then
    echo "FAIL: $label: expected DAML_FAILURE with \"$expected_message\"" >&2
    cat "$gg_work_dir/$label.err" >&2
    exit 1
  fi
  echo "PASS: $label rejected: DAML_FAILURE $expected_message"
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
expect_rejected old-set-governance "governance: not signed by current guardian set" relay_governance_vaa "old-set-governance-$run_sequence" \
  "$(jq -er .vaaHex <<<"$old_set_governance")"

echo "Step 5: VAA that names the new set, with old key $first_old_key at guardian index 0 (keys $forged_key_indices)"
forged_new_set="$(generate_vaa -kind message -signing-set-index "$new_set_index" -signing-key-indices "$forged_key_indices" -sequence "$run_sequence")"
expect_rejected new-set-index-old-keys "invalid signature for guardian index 0" \
  verify_vaa "new-set-index-old-keys-$run_sequence" \
  "$(jq -er .vaaHex <<<"$forged_new_set")"

if [[ "$new_set_quorum" -lt 2 ]]; then
  echo "Step 6: skipped. The new set has $new_set_size guardian; quorum $new_set_quorum leaves no quorum - 1 case."
  exit 0
fi
below_quorum_signers=$((new_set_quorum - 1))
below_quorum_positions="0"
for ((position = 1; position < below_quorum_signers; position++)); do
  below_quorum_positions+=",$position"
done
echo "Step 6: VAA of the new set signed by $below_quorum_signers of $new_set_size guardians (positions $below_quorum_positions, quorum $new_set_quorum)"
below_quorum_vaa="$(generate_vaa -kind message -signing-set-index "$new_set_index" -signing-key-indices "$new_key_indices" \
  -signer-positions "$below_quorum_positions" -sequence "$run_sequence")"
expect_rejected new-set-below-quorum "no quorum: have $below_quorum_signers, need $new_set_quorum" \
  verify_vaa "new-set-below-quorum-$run_sequence" \
  "$(jq -er .vaaHex <<<"$below_quorum_vaa")"
