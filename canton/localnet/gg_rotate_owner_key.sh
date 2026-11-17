#!/usr/bin/env bash
# G3 rehearsal: rotate one guardianGovernance owner key. The threshold stays 2 of 3.
#   1. Generate a new owner key. Remove <removed-owner> and add the new key in the decentralized
#      namespace and in the PartyToParticipant signing keys (console/gg_rotate_owner_key.canton).
#   2. Check that the removed owner and one kept owner cannot change the namespace.
#   3. Signing ceremony: create GovernanceCeremonyProbe as guardianGovernance.
#      a. One kept owner + the new owner. Expect commit.
#      b. One kept owner + the removed owner. Expect rejection.
#      c. The removed owner alone. Expect rejection.
# Usage: gg_rotate_owner_key.sh <removed-owner> <kept-owner> <kept-owner> <new-owner>
#   Owner names are key file prefixes under $LOCALNET_STATE_DIR/gg-keys, for example: owner3 owner1 owner2 owner4
# Precondition: gg_host.sh and gg_package_vetting.sh ran.
set -euo pipefail
# shellcheck source=gg_lib.sh
source "$(dirname "${BASH_SOURCE[0]}")/gg_lib.sh"

readonly KEY_TOOL="$REPO_ROOT/canton/devnet/guardian_key_tool.js"
readonly GG_KEY_DIR="$LOCALNET_STATE_DIR/gg-keys"
readonly EXTRA_FINGERPRINTS="$LOCALNET_STATE_DIR/extra-key-fingerprints.json"
readonly CEREMONY_TEMPLATE="#wormhole-localnet-vetting-probe:VettingProbe:GovernanceCeremonyProbe"

if [[ $# -ne 4 ]]; then
  echo "usage: gg_rotate_owner_key.sh <removed-owner> <kept-owner> <kept-owner> <new-owner>" >&2
  exit 2
fi
removed_owner="$1" kept_owner_1="$2" kept_owner_2="$3" new_owner="$4"
if [[ -f "$GG_KEY_DIR/$new_owner.key" ]]; then
  echo "$GG_KEY_DIR/$new_owner.key exists. Choose a new owner name." >&2
  exit 1
fi
node "$KEY_TOOL" generate "$GG_KEY_DIR/$new_owner" >/dev/null
# Canton key fingerprint: multihash prefix 1220 + SHA-256 of 0x0000000C (hash purpose 12) + the raw
# 32-byte Ed25519 key (the last 32 bytes of the DER SPKI).
new_owner_fingerprint="1220$( (printf '\x00\x00\x00\x0c'; tail -c 32 "$GG_KEY_DIR/$new_owner.pub") | openssl dgst -sha256 -r | cut -d' ' -f1)"

owner_sign_cmd() { echo "node $KEY_TOOL sign $GG_KEY_DIR/$1.key"; }
GG_PARTY_ID="$GG_PARTY" \
GG_KEPT_OWNER_PUBKEY_1="$GG_KEY_DIR/$kept_owner_1.pub" GG_KEPT_OWNER_SIGN_CMD_1="$(owner_sign_cmd "$kept_owner_1")" \
GG_KEPT_OWNER_PUBKEY_2="$GG_KEY_DIR/$kept_owner_2.pub" GG_KEPT_OWNER_SIGN_CMD_2="$(owner_sign_cmd "$kept_owner_2")" \
GG_REMOVED_OWNER_PUBKEY="$GG_KEY_DIR/$removed_owner.pub" GG_REMOVED_OWNER_SIGN_CMD="$(owner_sign_cmd "$removed_owner")" \
GG_ADDED_OWNER_PUBKEY="$GG_KEY_DIR/$new_owner.pub" GG_ADDED_OWNER_SIGN_CMD="$(owner_sign_cmd "$new_owner")" \
  "$LOCALNET_SCRIPT_DIR/canton_console.sh" "$LOCALNET_SCRIPT_DIR/console/gg_rotate_owner_key.canton" > "$gg_work_dir/rotation.log" 2>&1 || true
grep -E '^ROTATION' "$gg_work_dir/rotation.log" || true
echo "Removed-owner attempt error: $(grep -E "^ *(code|cause) = " "$gg_work_dir/rotation.log" | tail -n 2 | tr -s ' ' | tr '\n' ' ')"
if ! grep -q '^ROTATION done' "$gg_work_dir/rotation.log"; then
  echo "FAIL: owner-key rotation console script did not finish" >&2
  grep -E 'ERROR|error|Exception' "$gg_work_dir/rotation.log" | head -n 20 >&2
  exit 1
fi
if ! grep -q "ROTATION after: DN .*$new_owner_fingerprint" "$gg_work_dir/rotation.log"; then
  echo "FAIL: computed fingerprint $new_owner_fingerprint of $new_owner is not in the namespace" >&2
  exit 1
fi
updated_fingerprints="$(jq --arg file "$GG_KEY_DIR/$new_owner.pub" --arg fp "$new_owner_fingerprint" '.[$file] = $fp' "$EXTRA_FINGERPRINTS" 2>/dev/null \
  || jq -n --arg file "$GG_KEY_DIR/$new_owner.pub" --arg fp "$new_owner_fingerprint" '{($file): $fp}')"
printf '%s\n' "$updated_fingerprints" > "$EXTRA_FINGERPRINTS"
echo "PASS: $removed_owner replaced by $new_owner ($new_owner_fingerprint)"

create_ceremony_probe() {
  local label="$1"
  shift
  submit_interactive "ceremony-$label-$(date +%s)" "$(jq -n --arg gg "$GG_PARTY" '[$gg]')" \
    "$(jq -n --arg templateId "$CEREMONY_TEMPLATE" --arg gg "$GG_PARTY" --arg label "$label" \
      '{CreateCommand: {templateId: $templateId, createArguments: {guardianGovernance: $gg, label: $label}}}')" \
    "$(gg_signers_json "$@")"
}

echo "Step 3a: $kept_owner_1 + $new_owner"
ceremony_transaction="$(create_ceremony_probe kept-and-new "$kept_owner_1" "$new_owner")"
echo "PASS: committed, update $(jq -r .updateId <<<"$ceremony_transaction")"

for signer_set in "$kept_owner_1 $removed_owner" "$removed_owner"; do
  # shellcheck disable=SC2086 # signer_set is a word list of owner names
  echo "Step 3: $signer_set"
  # shellcheck disable=SC2086
  if create_ceremony_probe "${signer_set// /-}" $signer_set > "$gg_work_dir/ceremony.out" 2> "$gg_work_dir/ceremony.err"; then
    echo "FAIL: guardianGovernance transaction committed with signers: $signer_set" >&2
    exit 1
  fi
  echo "PASS: rejected: $(grep -v '^prepared' "$gg_work_dir/ceremony.err" | grep -oE '"cause":"[^"]*"' | head -n 1)"
done
