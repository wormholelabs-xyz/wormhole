#!/usr/bin/env bash
# Upload the NTT DAR, token-cip0056 and the vendored CIP-56 interface DARs to all three LocalNet
# participants and vet every package in them. guardianGovernance hosts must vet each NTT package.
# Usage: ntt_upload_dars.sh
# Precondition: the NTT repo checkout (NTT_REPO_DIR) has a built canton/ntt/.daml/dist/ntt-0.2.0.dar.
set -euo pipefail
# shellcheck source=env.sh
source "$(dirname "${BASH_SOURCE[0]}")/env.sh"

readonly NTT_CANTON_DIR="${NTT_REPO_DIR:-$(cd "$REPO_ROOT/.." && pwd)/ntt}/canton"
readonly NTT_DAR_PATHS=(
  "$NTT_CANTON_DIR/dars/splice-api-token-metadata-v1-1.0.0.dar"
  "$NTT_CANTON_DIR/dars/splice-api-token-holding-v1-1.0.0.dar"
  "$NTT_CANTON_DIR/dars/splice-api-token-allocation-v1-1.0.0.dar"
  "$NTT_CANTON_DIR/dars/splice-api-token-allocation-instruction-v1-1.0.0.dar"
  "$NTT_CANTON_DIR/dars/splice-api-token-transfer-instruction-v1-1.0.0.dar"
  "$NTT_CANTON_DIR/dars/splice-api-token-burn-mint-v1-1.0.0.dar"
  "$NTT_CANTON_DIR/dars/token-cip0056-0.2.0.dar"
  "$NTT_CANTON_DIR/ntt/.daml/dist/ntt-0.2.0.dar"
)
# Package id of the wormhole-core 0.6.0 DALF inside the NTT DAR. It must equal the core on LocalNet.
readonly PINNED_CORE_PACKAGE_ID="701a57c867e02d962f231bd9f0ff5186e16462a77289aa1b7aaba7fe95074c15"
# Package id of token-cip0056 0.2.0, whose CoinFactory template is the canonical burn/mint factory.
readonly PINNED_TOKEN_CIP0056_PACKAGE_ID="0a624d6a07264860159e881ca7ff6b349f58e95f393c8861529089f9cd8ba474"

for dar_path in "${NTT_DAR_PATHS[@]}"; do
  if [[ ! -f "$dar_path" ]]; then
    echo "DAR not found: $dar_path (build it with 'dpm build --all' in $NTT_CANTON_DIR)" >&2
    exit 1
  fi
done
ntt_dar="${NTT_DAR_PATHS[${#NTT_DAR_PATHS[@]} - 1]}"
if ! unzip -Z1 "$ntt_dar" | grep -q "wormhole-core-0.6.0-$PINNED_CORE_PACKAGE_ID.dalf"; then
  echo "$ntt_dar does not carry wormhole-core $PINNED_CORE_PACKAGE_ID" >&2
  exit 1
fi
ntt_package_id="$(unzip -Z1 "$ntt_dar" | sed -nE 's#^ntt-0\.2\.0-([0-9a-f]{64})/ntt-0\.2\.0-[0-9a-f]{64}\.dalf$#\1#p')"
if [[ ! "$ntt_package_id" =~ ^[0-9a-f]{64}$ ]]; then
  echo "Cannot read the ntt package id from $ntt_dar" >&2
  exit 1
fi

access_token="$("$LOCALNET_SCRIPT_DIR/token.sh")"
for validator_name in app-provider sv app-user; do
  ledger_json_url="$(json_api_url "$validator_name")"
  for dar_path in "${NTT_DAR_PATHS[@]}"; do
    http_status="$(curl -sS -o /dev/null -w '%{http_code}' \
      -X POST "$ledger_json_url/v2/dars?vetAllPackages=true" \
      -H "Authorization: Bearer $access_token" \
      -H 'Content-Type: application/octet-stream' \
      --data-binary "@$dar_path")"
    if [[ "$http_status" != "200" ]]; then
      echo "Upload of $(basename "$dar_path") to $validator_name failed with HTTP $http_status" >&2
      exit 1
    fi
  done
  participant_id="$(curl -sSf "$ledger_json_url/v2/parties/participant-id" -H "Authorization: Bearer $access_token" | jq -r .participantId)"
  for package_id in "$ntt_package_id" "$PINNED_CORE_PACKAGE_ID" "$PINNED_TOKEN_CIP0056_PACKAGE_ID"; do
    vetted_count="$(curl -sSf -X POST "$ledger_json_url/v2/package-vetting/list" -H "Authorization: Bearer $access_token" \
      -H 'Content-Type: application/json' \
      -d "$(jq -n --arg pid "$participant_id" '{topologyStateFilter: {participantIds: [$pid]}}')" \
      | jq --arg package_id "$package_id" '[.vettedPackages[]?.packages[] | select(.packageId == $package_id)] | length')"
    if [[ "$vetted_count" -lt 1 ]]; then
      echo "$validator_name does not vet package $package_id" >&2
      exit 1
    fi
  done
  echo "$validator_name: uploaded ${#NTT_DAR_PATHS[@]} DARs; ntt $ntt_package_id, wormhole-core and token-cip0056 vetted"
done
