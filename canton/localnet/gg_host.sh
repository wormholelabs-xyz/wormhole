#!/usr/bin/env bash
# Host guardianGovernance (decentralized namespace, 3 Ed25519 owner keys, threshold 2) on all three
# LocalNet participants at Confirmation with confirmation threshold 2. Upload and vet the core DARs on all three
# participants. Allocate the local party LabsOperator on app-provider (the operator: genesis offer, onboarding fee
# admin), and the local parties CorePublisher, CoreVerifier and GovernanceRelayer on sv.
# Usage: gg_host.sh
# Output: $LOCALNET_STATE_DIR/gg-artifacts.json, local-parties.json and the owner keys under $LOCALNET_STATE_DIR.
set -euo pipefail
# shellcheck source=env.sh
source "$(dirname "${BASH_SOURCE[0]}")/env.sh"

readonly DEVNET_DIR="$REPO_ROOT/canton/devnet"
readonly KEY_TOOL="$DEVNET_DIR/guardian_key_tool.js"
readonly GG_HOSTS="app-provider,sv,app-user"
readonly GG_ARTIFACTS="$LOCALNET_STATE_DIR/gg-artifacts.json"
readonly LOCAL_PARTIES="$LOCALNET_STATE_DIR/local-parties.json"

if [[ -f "$GG_ARTIFACTS" || -f "$LOCAL_PARTIES" ]]; then
  echo "Artifacts already exist in $LOCALNET_STATE_DIR. Remove that directory to host new parties." >&2
  exit 1
fi
mkdir -p "$LOCALNET_STATE_DIR/gg-keys"

for validator_name in app-provider sv app-user; do
  "$LOCALNET_SCRIPT_DIR/upload_dars.sh" "$validator_name"
  "$LOCALNET_SCRIPT_DIR/create_script_user.sh" "$validator_name"
done

for owner_index in 1 2 3; do
  node "$KEY_TOOL" generate "$LOCALNET_STATE_DIR/gg-keys/owner$owner_index" >/dev/null
done

# guardian_governance.canton targets the Canton 3.5 console of dpm. Canton 3.7 moved NonEmpty.
readonly CONSOLE_SCRIPT="$LOCALNET_STATE_DIR/guardian_governance.canton"
sed 's/^import com\.daml\.nonempty\.NonEmpty$/import com.digitalasset.nonempty.NonEmpty/' \
  "$DEVNET_DIR/guardian_governance.canton" > "$CONSOLE_SCRIPT"
grep -qx 'import com.digitalasset.nonempty.NonEmpty' "$CONSOLE_SCRIPT" \
  || { echo "the NonEmpty import patch did not apply to $CONSOLE_SCRIPT" >&2; exit 1; }
if grep -q 'com\.daml\.nonempty' "$CONSOLE_SCRIPT"; then
  echo "$CONSOLE_SCRIPT still imports com.daml.nonempty" >&2
  exit 1
fi

run_party_setup() {
  local party_name="$1" key_dir="$2" key_prefixes=("$3" "$4" "$5") threshold="$6" hosts="$7"
  local confirmation_threshold="$8" artifacts_file="$9"
  local owner_slot
  for owner_slot in 1 2 3; do
    export "GG_OWNER_PUBKEY_$owner_slot=$key_dir/${key_prefixes[$((owner_slot - 1))]}.pub"
    export "GG_OWNER_SIGN_ARGV_$owner_slot=$(jq -cn --arg tool "$KEY_TOOL" \
      --arg key "$key_dir/${key_prefixes[$((owner_slot - 1))]}.key" '["node", $tool, "sign", $key]')"
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

admin_auth_header="$(mktemp)"
trap 'rm -f "$admin_auth_header"' EXIT
write_auth_header_file "$admin_auth_header"
# allocate_local_party <validator> <party-hint>: allocate a party on that participant and print its id.
allocate_local_party() {
  curl --connect-timeout 10 --max-time 120 -sSf -X POST "$(json_api_url "$1")/v2/parties" -H "@$admin_auth_header" \
    -H 'Content-Type: application/json' -d "$(jq -n --arg hint "$2" '{partyIdHint: $hint, identityProviderId: ""}')" \
    | jq -er .partyDetails.party
}
labs_operator="$(allocate_local_party app-provider LabsOperator)"
core_publisher="$(allocate_local_party sv CorePublisher)"
core_verifier="$(allocate_local_party sv CoreVerifier)"
governance_relayer="$(allocate_local_party sv GovernanceRelayer)"
jq -n --arg labsOperator "$labs_operator" --arg corePublisher "$core_publisher" --arg coreVerifier "$core_verifier" \
  --arg governanceRelayer "$governance_relayer" \
  '{labsOperator: $labsOperator, corePublisher: $corePublisher, coreVerifier: $coreVerifier, governanceRelayer: $governanceRelayer}' \
  > "$LOCAL_PARTIES"

jq . "$GG_ARTIFACTS" "$LOCAL_PARTIES"
