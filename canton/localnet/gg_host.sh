#!/usr/bin/env bash
# Host guardianGovernance (decentralized namespace, 3 Ed25519 owner keys, threshold 2) on all three
# LocalNet participants at Confirmation with confirmation threshold 2. Host a 1-of-1 external
# Operator party on app-provider. Upload and vet the core DARs on all three participants.
# Usage: gg_host.sh
# Output: $LOCALNET_STATE_DIR/{gg,operator}-artifacts.json and the owner keys under $LOCALNET_STATE_DIR.
set -euo pipefail
# shellcheck source=env.sh
source "$(dirname "${BASH_SOURCE[0]}")/env.sh"

readonly DEVNET_DIR="$REPO_ROOT/canton/devnet"
readonly KEY_TOOL="$DEVNET_DIR/guardian_key_tool.js"
readonly GG_HOSTS="app-provider,sv,app-user"
readonly GG_ARTIFACTS="$LOCALNET_STATE_DIR/gg-artifacts.json"
readonly OPERATOR_ARTIFACTS="$LOCALNET_STATE_DIR/operator-artifacts.json"

if [[ -f "$GG_ARTIFACTS" || -f "$OPERATOR_ARTIFACTS" ]]; then
  echo "Artifacts already exist in $LOCALNET_STATE_DIR. Remove that directory to host new parties." >&2
  exit 1
fi
mkdir -p "$LOCALNET_STATE_DIR/gg-keys" "$LOCALNET_STATE_DIR/operator-keys"

for validator_name in app-provider sv app-user; do
  "$LOCALNET_SCRIPT_DIR/upload_dars.sh" "$validator_name"
  "$LOCALNET_SCRIPT_DIR/create_script_user.sh" "$validator_name"
done

for owner_index in 1 2 3; do
  node "$KEY_TOOL" generate "$LOCALNET_STATE_DIR/gg-keys/owner$owner_index" >/dev/null
done
node "$KEY_TOOL" generate "$LOCALNET_STATE_DIR/operator-keys/owner1" >/dev/null

# guardian_governance.canton targets the Canton 3.5 console of dpm. Canton 3.7 moved NonEmpty.
readonly CONSOLE_SCRIPT="$LOCALNET_STATE_DIR/guardian_governance.canton"
sed 's/^import com\.daml\.nonempty\.NonEmpty$/import com.digitalasset.nonempty.NonEmpty/' \
  "$DEVNET_DIR/guardian_governance.canton" > "$CONSOLE_SCRIPT"

run_party_setup() {
  local party_name="$1" key_dir="$2" key_prefixes=("$3" "$4" "$5") threshold="$6" hosts="$7"
  local confirmation_threshold="$8" artifacts_file="$9"
  local owner_slot
  for owner_slot in 1 2 3; do
    export "GG_OWNER_PUBKEY_$owner_slot=$key_dir/${key_prefixes[$((owner_slot - 1))]}.pub"
    export "GG_OWNER_SIGN_CMD_$owner_slot=node $KEY_TOOL sign $key_dir/${key_prefixes[$((owner_slot - 1))]}.key"
  done
  GG_PARTY_NAME="$party_name" GG_THRESHOLD="$threshold" GG_HOSTING_PARTICIPANTS="$hosts" \
    GG_CONFIRMATION_THRESHOLD="$confirmation_threshold" GG_SYNCHRONIZER_ALIAS=global \
    GG_ARTIFACTS_FILE="$artifacts_file" \
    "$LOCALNET_SCRIPT_DIR/canton_console.sh" "$CONSOLE_SCRIPT"
  if [[ ! -f "$artifacts_file" ]]; then
    echo "Console script did not write $artifacts_file" >&2
    exit 1
  fi
}

run_party_setup guardianGovernance "$LOCALNET_STATE_DIR/gg-keys" owner1 owner2 owner3 2 "$GG_HOSTS" 2 "$GG_ARTIFACTS"
# One owner key in all three slots gives a 1-of-1 namespace.
run_party_setup Operator "$LOCALNET_STATE_DIR/operator-keys" owner1 owner1 owner1 1 app-provider 1 "$OPERATOR_ARTIFACTS"

jq . "$GG_ARTIFACTS" "$OPERATOR_ARTIFACTS"
