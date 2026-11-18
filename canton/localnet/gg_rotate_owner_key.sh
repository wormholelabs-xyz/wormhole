#!/usr/bin/env bash
# G3 rehearsal: rotate one owner key of the guardian namespace. The thresholds stay 2 of 3.
# The rotation covers guardianGovernance and, after custody_host.sh, nttCustody (same namespace, same keys).
#   1. Generate a new owner key. console/gg_rotate_owner_key.canton computes each intended change from the topology
#      that all console participants agree on. It compares each generated change with the intended change, then signs
#      each change (sign phase). It stores the signed changes (mode 0600), then loads the PartyToParticipant of each
#      party (nttCustody first) and the namespace in one call.
#   2. The console reads all participants again. The namespace owners are the new set, and the removed key is in no
#      key set of the namespace. ROTATION done removes the stored file.
#   3. Signing ceremony, for each party: create GovernanceCeremonyProbe with that party as signatory.
#      a. One kept owner + the new owner. Expect commit.
#      b. One kept owner + the removed owner. Expect FAILED_TO_EXECUTE_TRANSACTION (1 valid signature).
#      c. The removed owner alone. Expect FAILED_TO_EXECUTE_TRANSACTION (0 valid signatures).
# Usage: gg_rotate_owner_key.sh [--resume] <removed-owner> <kept-owner> <kept-owner> <new-owner>
#   --resume finishes a half-applied rotation. It loads the stored signed changes and calls no signer for them.
#   Owner names are key file prefixes under $LOCALNET_STATE_DIR/gg-keys, for example: owner3 owner1 owner2 owner4
# Rehearsal faults (each prints ROTATION FAULT INJECTION ACTIVE):
#   GG_ROTATION_FAULT_SIGNER_OF=<owner name>               The signer of that owner exits 1.
#   GG_ROTATION_FAULT_OMIT_PARTY_OF=<party name>           The load omits the change of that party.
#   GG_ROTATION_FAULT_UNSIGNED_BY_ADDED_OF=<party name>    The load sends the change of that party without the
#                                                          signature of the added owner (measurement R4).
#   GG_ROTATION_MEASURE_SINGLE_LOAD=true                   The root delegation goes into the same load call (R5).
#   GG_ROTATION_FAULT_PARTY_IDS=<party id>,...             Replaces the rotation party list (safety stop SS1, SS3).
# Precondition: gg_host.sh and gg_package_vetting.sh ran.
set -euo pipefail
# shellcheck source=gg_lib.sh
source "$(dirname "${BASH_SOURCE[0]}")/gg_lib.sh"
print_active_rotation_faults >&2

readonly KEY_TOOL="$REPO_ROOT/canton/devnet/guardian_key_tool.js"
readonly GG_KEY_DIR="$LOCALNET_STATE_DIR/gg-keys"
readonly EXTRA_FINGERPRINTS="$LOCALNET_STATE_DIR/extra-key-fingerprints.json"
# Worst case of one console run is about 1030 s plus read time: 12 signer calls of 30 s, two reads of 60 s for the
# participant views to agree, load 1 of 120 s, and three load 2 tries of 120 s each with two 5 s pauses.
# canton_console.sh accepts at most 3600 s.
readonly ROTATION_CONSOLE_TIMEOUT_SECONDS=1200

readonly USAGE="usage: gg_rotate_owner_key.sh [--resume] <removed-owner> <kept-owner> <kept-owner> <new-owner>"
resume_rotation=false
if [[ "${1:-}" == "--resume" ]]; then
  resume_rotation=true
  shift
fi
if [[ $# -ne 4 ]]; then
  echo "$USAGE" >&2
  exit 2
fi
removed_owner="$1" kept_owner_1="$2" kept_owner_2="$3" new_owner="$4"
signer_fault_owner="${GG_ROTATION_FAULT_SIGNER_OF:-}"
if [[ -n "$signer_fault_owner" ]]; then
  check_owner_names_are_safe "$signer_fault_owner"
fi
readonly SIGNED_ROTATION_TRANSACTIONS="$LOCALNET_STATE_DIR/rotation-$new_owner.signed.bin"
check_owner_names_are_safe "$@"
check_owner_names_differ "$@"
for existing_owner in "$removed_owner" "$kept_owner_1" "$kept_owner_2"; do
  for key_file in "$GG_KEY_DIR/$existing_owner.pub" "$GG_KEY_DIR/$existing_owner.key"; do
    if [[ ! -f "$key_file" ]]; then
      echo "Missing $key_file" >&2
      exit 1
    fi
  done
done
if [[ "$resume_rotation" == true ]]; then
  rotation_mode=resume
  if [[ ! -f "$GG_KEY_DIR/$new_owner.key" || ! -f "$GG_KEY_DIR/$new_owner.pub" ]]; then
    echo "--resume needs the key of $new_owner from the interrupted run in $GG_KEY_DIR" >&2
    exit 1
  fi
  if [[ ! -f "$SIGNED_ROTATION_TRANSACTIONS" ]]; then
    echo "--resume needs the signed changes $SIGNED_ROTATION_TRANSACTIONS of the interrupted run" >&2
    exit 1
  fi
  echo "Resume the rotation from $SIGNED_ROTATION_TRANSACTIONS"
else
  rotation_mode=fresh
  if [[ -f "$GG_KEY_DIR/$new_owner.key" ]]; then
    echo "$GG_KEY_DIR/$new_owner.key exists. Choose a new owner name, or pass --resume to finish an interrupted rotation." >&2
    exit 1
  fi
  if [[ -f "$SIGNED_ROTATION_TRANSACTIONS" ]]; then
    echo "$SIGNED_ROTATION_TRANSACTIONS exists. Pass --resume, or choose a new owner name." >&2
    exit 1
  fi
  node "$KEY_TOOL" generate "$GG_KEY_DIR/$new_owner" >/dev/null
fi
# Canton key fingerprint: multihash prefix 1220 + SHA-256 of 0x0000000C (hash purpose 12) + the raw
# 32-byte Ed25519 key (the last 32 bytes of the DER SPKI).
new_owner_fingerprint="1220$( (printf '\x00\x00\x00\x0c'; tail -c 32 "$GG_KEY_DIR/$new_owner.pub") | openssl dgst -sha256 -r | cut -d' ' -f1)"
if [[ -f "$EXTRA_FINGERPRINTS" ]]; then
  updated_fingerprints="$(jq -e --arg file "$GG_KEY_DIR/$new_owner.pub" --arg fp "$new_owner_fingerprint" \
    'if type == "object" then .[$file] = $fp else error("not a JSON object") end' "$EXTRA_FINGERPRINTS")" \
    || { echo "FAIL: $EXTRA_FINGERPRINTS is damaged. Fix or remove it, then rerun with --resume." >&2; exit 1; }
else
  updated_fingerprints="$(jq -n --arg file "$GG_KEY_DIR/$new_owner.pub" --arg fp "$new_owner_fingerprint" '{($file): $fp}')"
fi
fingerprints_temporary_file="$(mktemp "$EXTRA_FINGERPRINTS.XXXXXX")"
printf '%s\n' "$updated_fingerprints" > "$fingerprints_temporary_file"
mv -f "$fingerprints_temporary_file" "$EXTRA_FINGERPRINTS"

# Load order: nttCustody (holds the reserves) first, guardianGovernance last.
rotation_party_ids=()
if [[ -n "${GG_ROTATION_FAULT_PARTY_IDS:-}" ]]; then
  IFS=, read -r -a rotation_party_ids <<<"$GG_ROTATION_FAULT_PARTY_IDS"
else
  if [[ -n "$CUSTODY_PARTY" ]]; then
    rotation_party_ids+=("$CUSTODY_PARTY")
  fi
  rotation_party_ids+=("$GG_PARTY")
fi
expected_party_order="$(for party_id in "${rotation_party_ids[@]}"; do echo "${party_id%%::*}"; done | paste -sd, -)"
echo "Parties to rotate, in load order: $expected_party_order"

# owner_signer_argv <owner>: JSON argv of the signer of that owner key; a space in the path stays in one argument.
owner_signer_argv() {
  if [[ "$1" == "$signer_fault_owner" ]]; then
    jq -cn --arg owner "$1" '["/bin/sh", "-c", "echo injected signer fault of \($owner) >&2; exit 1", "injected-signer-fault"]'
  else
    jq -cn --arg tool "$KEY_TOOL" --arg key "$GG_KEY_DIR/$1.key" '["node", $tool, "sign", $key]'
  fi
}
rotation_console_status=0
GG_PARTY_ID="$GG_PARTY" GG_ROTATION_PARTY_IDS="$(IFS=,; echo "${rotation_party_ids[*]}")" \
GG_ROTATION_MODE="$rotation_mode" GG_ROTATION_SIGNED_TRANSACTIONS_FILE="$SIGNED_ROTATION_TRANSACTIONS" \
GG_KEPT_OWNER_PUBKEY_1="$GG_KEY_DIR/$kept_owner_1.pub" GG_KEPT_OWNER_SIGN_ARGV_1="$(owner_signer_argv "$kept_owner_1")" \
GG_KEPT_OWNER_PUBKEY_2="$GG_KEY_DIR/$kept_owner_2.pub" GG_KEPT_OWNER_SIGN_ARGV_2="$(owner_signer_argv "$kept_owner_2")" \
GG_REMOVED_OWNER_PUBKEY="$GG_KEY_DIR/$removed_owner.pub" \
GG_ADDED_OWNER_PUBKEY="$GG_KEY_DIR/$new_owner.pub" GG_ADDED_OWNER_SIGN_ARGV="$(owner_signer_argv "$new_owner")" \
CANTON_CONSOLE_TIMEOUT_SECONDS="$ROTATION_CONSOLE_TIMEOUT_SECONDS" \
  "$LOCALNET_SCRIPT_DIR/canton_console.sh" "$OWNER_SIGNING_PRELUDE" "$LOCALNET_SCRIPT_DIR/console/owner_key_rotation_lib.canton" \
  "$LOCALNET_SCRIPT_DIR/console/gg_rotate_owner_key.canton" \
  > "$gg_work_dir/rotation.log" 2>&1 || rotation_console_status=$?
grep -E '^ROTATION' "$gg_work_dir/rotation.log" || true
recovery_command="$0 --resume $removed_owner $kept_owner_1 $kept_owner_2 $new_owner"
if grep -q 'SAFETY STOP:' "$gg_work_dir/rotation.log" && ! grep -qxF 'ROTATION safety stop passed' "$gg_work_dir/rotation.log"; then
  echo "FAIL: safety stop before any change. $(grep -oE 'SAFETY STOP:.*' "$gg_work_dir/rotation.log" | head -n 1)" >&2
  exit 1
fi
if grep -q '^ROTATION NOT STARTED' "$gg_work_dir/rotation.log"; then
  echo "FAIL: the sign phase failed. The topology did not change. Run again with a new owner name." >&2
  exit 1
fi
if grep -q '^ROTATION NOT APPLIED' "$gg_work_dir/rotation.log"; then
  echo "FAIL: the participant applied no change. The removed key stays valid. Run again with a new owner name." >&2
  echo "WARNING: $SIGNED_ROTATION_TRANSACTIONS stays loadable until another change takes its serials. Keep it secret." >&2
  exit 1
fi
if grep -q '^ROTATION HALF-APPLIED' "$gg_work_dir/rotation.log"; then
  echo "FAIL: half-applied rotation. The removed key stays valid for the parties in the ROTATION HALF-APPLIED line." >&2
  echo "Recover now with: $recovery_command" >&2
  exit 1
fi
if grep -q '^ROTATION applied' "$gg_work_dir/rotation.log" && ! grep -q '^ROTATION done' "$gg_work_dir/rotation.log"; then
  echo "FAIL: each participant shows the new key sets, but a check after the load failed (status $rotation_console_status)." >&2
  grep -E 'ERROR|error|Exception' "$gg_work_dir/rotation.log" | head -n 20 >&2
  echo "Do not run --resume: it repeats the same checks. Examine the topology with gg_show_topology.sh." >&2
  echo "Each change in $SIGNED_ROTATION_TRANSACTIONS is effective. Remove the file after the examination." >&2
  exit 1
fi
if [[ "$rotation_console_status" -ne 0 ]] || ! grep -q '^ROTATION done' "$gg_work_dir/rotation.log"; then
  echo "FAIL: owner-key rotation console script did not finish (status $rotation_console_status)" >&2
  grep -E 'ERROR|error|Exception' "$gg_work_dir/rotation.log" | head -n 20 >&2
  echo "If the output shows a load attempt, check the topology with gg_show_topology.sh, then recover with: $recovery_command" >&2
  exit 1
fi
if ! grep -qxF "ROTATION parties: $expected_party_order" "$gg_work_dir/rotation.log"; then
  echo "FAIL: the console did not rotate the parties in the order $expected_party_order" >&2
  exit 1
fi
if ! grep -qxF "ROTATION protocol version: 37" "$gg_work_dir/rotation.log"; then
  echo "FAIL: the console did not report protocol version 37" >&2
  exit 1
fi
if ! grep -qE '^ROTATION effective times \((one load call|loaded in more than one call)\): ' "$gg_work_dir/rotation.log"; then
  echo "FAIL: the console did not report the effective time of the changes" >&2
  exit 1
fi
if ! grep -q "ROTATION after: DN .*$new_owner_fingerprint" "$gg_work_dir/rotation.log"; then
  echo "FAIL: computed fingerprint $new_owner_fingerprint of $new_owner is not in the namespace" >&2
  exit 1
fi
if ! grep -qE "^ROTATION removed owner key [0-9a-f]+ is in no key set of the namespace$" "$gg_work_dir/rotation.log"; then
  echo "FAIL: the console did not confirm that the removed key is in no key set of the namespace" >&2
  exit 1
fi
echo "PASS: $removed_owner replaced by $new_owner ($new_owner_fingerprint)"
for party_id in "${rotation_party_ids[@]}"; do
  if ! grep -q "^ROTATION after: PTP party=${party_id%%::*} .*signingKeys=2-of-3" "$gg_work_dir/rotation.log"; then
    echo "FAIL: no rotated PTP signing keys for ${party_id%%::*}" >&2
    exit 1
  fi
done

# expect_ceremony_rejected <party-id> <step> <valid-signature-count> <owner>...: the 2-of-3 party signature check
# must fail with FAILED_TO_EXECUTE_TRANSACTION, one invalid signature (the removed owner) and the given valid count.
expect_ceremony_rejected() {
  local party_id="$1" step="$2" valid_signature_count="$3" expected_message
  shift 3
  echo "Step $step (${party_id%%::*}): $*"
  if create_ceremony_probe "$party_id" "$(IFS=-; echo "$*")" "$@" > "$gg_work_dir/ceremony.out" 2> "$gg_work_dir/ceremony.err"; then
    echo "FAIL: ${party_id%%::*} transaction committed with signers: $*" >&2
    exit 1
  fi
  expected_message="Received $valid_signature_count valid signatures from distinct keys (1 invalid), but expected at least 2 valid for ${party_id%%::*}::"
  if ! grep -qF 'FAILED_TO_EXECUTE_TRANSACTION' "$gg_work_dir/ceremony.err" \
      || ! grep -qF "$expected_message" "$gg_work_dir/ceremony.err"; then
    echo "FAIL: expected FAILED_TO_EXECUTE_TRANSACTION with: $expected_message" >&2
    cat "$gg_work_dir/ceremony.err" >&2
    exit 1
  fi
  echo "PASS: rejected: FAILED_TO_EXECUTE_TRANSACTION, $valid_signature_count valid + 1 invalid of 2 required"
}

for party_id in "${rotation_party_ids[@]}"; do
  echo "Step 3a (${party_id%%::*}): $kept_owner_1 + $new_owner"
  ceremony_transaction="$(create_ceremony_probe "$party_id" kept-and-new "$kept_owner_1" "$new_owner")"
  echo "PASS: committed, update $(jq -r .updateId <<<"$ceremony_transaction")"
  expect_ceremony_rejected "$party_id" 3b 1 "$kept_owner_1" "$removed_owner"
  expect_ceremony_rejected "$party_id" 3c 0 "$removed_owner"
done
