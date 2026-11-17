#!/usr/bin/env bash
# G3 rehearsal: rotate one guardianGovernance owner key. The threshold stays 2 of 3.
# The rotation covers guardianGovernance and, after custody_host.sh, nttCustody (same namespace, same keys).
#   1. Generate a new owner key. Remove <removed-owner> and add the new key in the decentralized
#      namespace and in the PartyToParticipant signing keys of each party (console/gg_rotate_owner_key.canton).
#   2. Check that the removed owner and one kept owner cannot change the namespace
#      (TOPOLOGY_UNAUTHORIZED_TRANSACTION).
#   3. Signing ceremony, for each party: create GovernanceCeremonyProbe with that party as signatory.
#      a. One kept owner + the new owner. Expect commit.
#      b. One kept owner + the removed owner. Expect FAILED_TO_EXECUTE_TRANSACTION (1 valid signature).
#      c. The removed owner alone. Expect FAILED_TO_EXECUTE_TRANSACTION (0 valid signatures).
# Usage: gg_rotate_owner_key.sh [--resume] <removed-owner> <kept-owner> <kept-owner> <new-owner>
#   --resume finishes an interrupted rotation with the existing key of <new-owner>.
#   GG_ROTATION_FAIL_BEFORE_SIGNING_KEYS=true stops the rotation after the namespace change, to rehearse --resume.
#   GG_ROTATION_FAIL_BEFORE_SIGNING_KEYS_OF=nttCustody stops it after the guardianGovernance signing keys.
#   Owner names are key file prefixes under $LOCALNET_STATE_DIR/gg-keys, for example: owner3 owner1 owner2 owner4
# Precondition: gg_host.sh and gg_package_vetting.sh ran.
set -euo pipefail
# shellcheck source=gg_lib.sh
source "$(dirname "${BASH_SOURCE[0]}")/gg_lib.sh"

readonly KEY_TOOL="$REPO_ROOT/canton/devnet/guardian_key_tool.js"
readonly GG_KEY_DIR="$LOCALNET_STATE_DIR/gg-keys"
readonly EXTRA_FINGERPRINTS="$LOCALNET_STATE_DIR/extra-key-fingerprints.json"

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
  if [[ ! -f "$GG_KEY_DIR/$new_owner.key" || ! -f "$GG_KEY_DIR/$new_owner.pub" ]]; then
    echo "--resume needs the key of $new_owner from the interrupted run in $GG_KEY_DIR" >&2
    exit 1
  fi
  echo "Resume the rotation with the existing key of $new_owner"
else
  if [[ -f "$GG_KEY_DIR/$new_owner.key" ]]; then
    echo "$GG_KEY_DIR/$new_owner.key exists. Choose a new owner name, or pass --resume to finish an interrupted rotation." >&2
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
printf '%s\n' "$updated_fingerprints" > "$EXTRA_FINGERPRINTS"

rotation_party_ids=("$GG_PARTY")
if [[ -n "$CUSTODY_PARTY" ]]; then
  rotation_party_ids+=("$CUSTODY_PARTY")
fi
echo "Parties to rotate: ${rotation_party_ids[*]%%::*}"

# owner_signer_argv <owner>: JSON argv of the signer of that owner key; a space in the path stays in one argument.
owner_signer_argv() { jq -cn --arg tool "$KEY_TOOL" --arg key "$GG_KEY_DIR/$1.key" '["node", $tool, "sign", $key]'; }
rotation_console_status=0
rotation_started_at="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
GG_PARTY_ID="$GG_PARTY" GG_ROTATION_PARTY_IDS="$(IFS=,; echo "${rotation_party_ids[*]}")" \
GG_KEPT_OWNER_PUBKEY_1="$GG_KEY_DIR/$kept_owner_1.pub" GG_KEPT_OWNER_SIGN_ARGV_1="$(owner_signer_argv "$kept_owner_1")" \
GG_KEPT_OWNER_PUBKEY_2="$GG_KEY_DIR/$kept_owner_2.pub" GG_KEPT_OWNER_SIGN_ARGV_2="$(owner_signer_argv "$kept_owner_2")" \
GG_REMOVED_OWNER_PUBKEY="$GG_KEY_DIR/$removed_owner.pub" GG_REMOVED_OWNER_SIGN_ARGV="$(owner_signer_argv "$removed_owner")" \
GG_ADDED_OWNER_PUBKEY="$GG_KEY_DIR/$new_owner.pub" GG_ADDED_OWNER_SIGN_ARGV="$(owner_signer_argv "$new_owner")" \
  "$LOCALNET_SCRIPT_DIR/canton_console.sh" "$OWNER_SIGNING_PRELUDE" "$LOCALNET_SCRIPT_DIR/console/gg_rotate_owner_key.canton" \
  > "$gg_work_dir/rotation.log" 2>&1 || rotation_console_status=$?
grep -E '^ROTATION' "$gg_work_dir/rotation.log" || true
if grep -q '^ROTATION HALF-APPLIED' "$gg_work_dir/rotation.log"; then
  # Show which key pairs sign for each party in the half-applied state.
  for party_id in "${rotation_party_ids[@]}"; do
    for probe_owner in "$new_owner" "$removed_owner"; do
      if create_ceremony_probe "$party_id" "half-applied-$probe_owner" "$kept_owner_1" "$probe_owner" > /dev/null 2> "$gg_work_dir/half-applied.err"; then
        echo "HALF-APPLIED STATE: $kept_owner_1 + $probe_owner sign for ${party_id%%::*}: committed"
      else
        echo "HALF-APPLIED STATE: $kept_owner_1 + $probe_owner sign for ${party_id%%::*}: rejected: $(grep -oE 'Received [0-9]+ valid signatures[^.]*' "$gg_work_dir/half-applied.err" | head -n 1)"
      fi
    done
  done
  echo "FAIL: half-applied rotation. Recover with: $0 --resume $removed_owner $kept_owner_1 $kept_owner_2 $new_owner" >&2
  exit 1
fi
if [[ "$rotation_console_status" -ne 0 ]] || ! grep -q '^ROTATION done' "$gg_work_dir/rotation.log"; then
  echo "FAIL: owner-key rotation console script did not finish (status $rotation_console_status)" >&2
  grep -E 'ERROR|error|Exception' "$gg_work_dir/rotation.log" | head -n 20 >&2
  echo "If the output shows 'ROTATION after: DN', recover with: $0 --resume $removed_owner $kept_owner_1 $kept_owner_2 $new_owner" >&2
  exit 1
fi
if ! grep -q "ROTATION after: DN .*$new_owner_fingerprint" "$gg_work_dir/rotation.log"; then
  echo "FAIL: computed fingerprint $new_owner_fingerprint of $new_owner is not in the namespace" >&2
  exit 1
fi
if ! grep -q '^ROTATION removed-owner attempt rejected by the API' "$gg_work_dir/rotation.log"; then
  echo "FAIL: the removed-owner namespace change was not rejected" >&2
  exit 1
fi
# The Admin API returns the rejection redacted, with the request trace id. The removed-owner attempt is the
# last failed request of the console run.
removed_owner_trace_id="$(grep -oE 'inquire about the request [0-9a-f]{32}' "$gg_work_dir/rotation.log" | tail -n 1 | awk '{print $NF}')"
if [[ ! "$removed_owner_trace_id" =~ ^[0-9a-f]{32}$ ]]; then
  echo "FAIL: no trace id of the removed-owner rejection in the console output" >&2
  exit 1
fi
removed_owner_rejection="$(docker logs canton --since "$rotation_started_at" 2>&1 \
  | grep -F "\"trace-id\":\"$removed_owner_trace_id\"" \
  | grep -F '"error-code":"TOPOLOGY_UNAUTHORIZED_TRANSACTION' \
  | jq -r '.message' || true)"
if [[ -z "$removed_owner_rejection" ]]; then
  echo "FAIL: the participant log has no TOPOLOGY_UNAUTHORIZED_TRANSACTION for trace id $removed_owner_trace_id" >&2
  exit 1
fi
echo "  participant log, trace id $removed_owner_trace_id: $removed_owner_rejection"
echo "PASS: removed-owner namespace change rejected with TOPOLOGY_UNAUTHORIZED_TRANSACTION"
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
