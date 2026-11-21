# Shared functions for the NTT LocalNet scripts. Source this file; do not run it.
# shellcheck shell=bash disable=SC2034
# shellcheck source=gg_lib.sh
source "$(dirname "${BASH_SOURCE[0]}")/gg_lib.sh"
# shellcheck source=ntt_offline_lib.sh
source "$(dirname "${BASH_SOURCE[0]}")/ntt_offline_lib.sh"

# Package pins of the NTT run. ntt_upload_dars.sh checks each DAR against them with ==.
# wormhole-core 0.12.3 (Daml SDK 3.7.3): the LocalNet core DAR, the core DALF of the NTT DAR and the watcher filter.
readonly WORMHOLE_CORE_VERSION="0.12.3"
readonly WORMHOLE_CORE_PACKAGE_ID="32abbcd2a6b5ebc2530f8e2cfc3fd1b853ed496443c6a0125fd2cea617b33371"
# NTT_PIN_VERSION selects one entry of ntt_pinned_package_id.
readonly NTT_VERSION="${NTT_PIN_VERSION:-0.6.1}"
NTT_PACKAGE_ID="$(ntt_pinned_package_id "$NTT_VERSION")" || exit 1
readonly NTT_PACKAGE_ID
# token-cip0056 0.2.0: its CoinFactory template is the canonical burn/mint factory.
readonly TOKEN_CIP0056_PACKAGE_ID="0a624d6a07264860159e881ca7ff6b349f58e95f393c8861529089f9cd8ba474"
readonly NTT_CANTON_DIR="${NTT_REPO_DIR:-$(cd "$REPO_ROOT/.." && pwd)/ntt}/canton"
readonly NTT_DAR="$NTT_CANTON_DIR/ntt/.daml/dist/ntt-$NTT_VERSION.dar"
# wormhole-genesis 0.2.1 on ntt 0.6.1 and core 0.12.2: NTT branch canton/migrate-ntt-layer, commit 16ce38d.
readonly WORMHOLE_GENESIS_VERSION="0.2.1"
readonly WORMHOLE_GENESIS_PACKAGE_ID="8af94ec56cd660812074f23437bbd798e1c13c6be18f512437a413bd9775096c"
readonly WORMHOLE_GENESIS_DAR="$NTT_CANTON_DIR/genesis/.daml/dist/wormhole-genesis-$WORMHOLE_GENESIS_VERSION.dar"

NTT_CUSTODY_ARTIFACTS="$LOCALNET_STATE_DIR/custody-artifacts.json"
NTT_CUSTODY_PARTY=""
if ntt_requires_custody_party "$NTT_VERSION"; then
  [[ -f "$NTT_CUSTODY_ARTIFACTS" ]] || fail "ntt $NTT_VERSION needs nttCustody: $NTT_CUSTODY_ARTIFACTS does not exist"
  NTT_CUSTODY_PARTY="$(jq -er .partyId "$NTT_CUSTODY_ARTIFACTS")"
  [[ "$NTT_CUSTODY_PARTY" == nttCustody::* ]] || fail "$NTT_CUSTODY_ARTIFACTS partyId is not an nttCustody party: $NTT_CUSTODY_PARTY"
fi

NTT_CONTRACTS="$LOCALNET_STATE_DIR/ntt-contracts.json"
NTT_KEY_DIR="$LOCALNET_STATE_DIR/ntt-keys"
# Two of the three current guardianGovernance owner keys (after gg_rotate_owner_key.sh: owner1, owner2, owner6).
read -r -a NTT_GG_SIGNING_OWNERS <<<"${NTT_GG_SIGNING_OWNERS:-owner1 owner2}"
# Current guardian set index and its devnet key indices (after gg_guardian_set_rotation.sh 0 1).
NTT_GUARDIAN_SET_INDEX="${NTT_GUARDIAN_SET_INDEX:-1}"
NTT_GUARDIAN_KEY_INDICES="${NTT_GUARDIAN_KEY_INDICES:-1}"
SCAN_URL="${SCAN_URL:-http://localhost:4000}"
SCAN_HOST_HEADER="Host: scan.localhost"

readonly NTT_GOVERNANCE_TEMPLATE="#ntt:Wormhole.Ntt.Governance:NttGovernance"
readonly NTT_MANAGER_TEMPLATE="#ntt:Wormhole.Ntt.Manager:NttManager"
readonly DEPOSIT_PREAPPROVAL_TEMPLATE="#ntt:Wormhole.Ntt.Deposit:DepositPreapproval"
readonly COIN_FACTORY_TEMPLATE="#token-cip0056:Token.CIP0056.CoinFactory:CoinFactory"
readonly EMPTY_EXTRA_ARGS='{"context": {"values": {}}, "meta": {"values": {}}}'

mkdir -p "$NTT_KEY_DIR"

gg_ceremony_signers_json() {
  gg_signers_json "${NTT_GG_SIGNING_OWNERS[@]}"
}

# ntt_custody_signers_json: nttCustody signatures. Its signing keys are the guardianGovernance owner keys.
ntt_custody_signers_json() {
  [[ -n "$NTT_CUSTODY_PARTY" ]] || fail "ntt_custody_signers_json: ntt $NTT_VERSION has no nttCustody party"
  gg_ceremony_signers_json | jq -e --arg party "$NTT_CUSTODY_PARTY" \
    'if length == 1 then {($party): (to_entries[0].value)} else error("expected one guardianGovernance signer entry, found \(length)") end'
}

# submit_local_expect_rejection <validator> <command-id> <act-as-json-array> <command-json> <disclosed-json> <expected-error-text>
# Fails if the participant accepts the command, or if the rejection lacks the expected text.
submit_local_expect_rejection() {
  local response
  response="$(submit_local_response "$1" "$2" "$3" "$4" "$5")" || return 1
  classify_submit_response "$response" "$6" || return 1
  echo "$2 rejected on $1 with '$6'"
}

# Wallet (validator) API, as the wallet user. validator_api_url <validator>
validator_api_url() {
  echo "http://localhost:$(validator_port_prefix "$1")903/api/validator/v0"
}

# wallet_onboard <validator> <user>: onboard the user and print its party id.
wallet_onboard() {
  curl --max-time 120 -sSf -X POST "$(validator_api_url "$1")/register" -H "@$(subject_auth_header_file "$2")" \
    -H 'Content-Type: application/json' -d '{}' | jq -er .party_id
}

wallet_tap() {
  curl --max-time 120 -sSf -X POST "$(validator_api_url "$1")/wallet/tap" -H "@$(subject_auth_header_file "$2")" \
    -H 'Content-Type: application/json' -d "$(jq -n --arg amount "$3" '{amount: $amount}')" >/dev/null
}

# wallet_create_transfer_preapproval <validator> <user>: print the TransferPreapproval contract id.
wallet_create_transfer_preapproval() {
  curl --max-time 120 -sSf -X POST "$(validator_api_url "$1")/wallet/transfer-preapproval" \
    -H "@$(subject_auth_header_file "$2")" -H 'Content-Type: application/json' -d '{}' \
    | jq -er .transfer_preapproval_contract_id
}

dso_party() {
  curl --max-time 120 -sSf -H "$SCAN_HOST_HEADER" "$SCAN_URL/api/scan/v0/dso-party-id" | jq -er .dso_party_id
}

# amulet_transfer_factory <sender> <receiver> <amount> <input-holding-cids-json>: the registry's factory,
# transfer kind and choice context for an Amulet transfer.
amulet_transfer_factory() {
  local dso now later
  dso="$(dso_party)"
  now="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  later="$(date -u -v+1H +%Y-%m-%dT%H:%M:%SZ 2>/dev/null || date -u -d '+1 hour' +%Y-%m-%dT%H:%M:%SZ)"
  curl --max-time 120 -sSf -X POST -H "$SCAN_HOST_HEADER" -H 'Content-Type: application/json' \
    "$SCAN_URL/registry/transfer-instruction/v1/transfer-factory" \
    -d "$(jq -n --arg dso "$dso" --arg sender "$1" --arg receiver "$2" --arg amount "$3" --argjson inputs "$4" \
      --arg now "$now" --arg later "$later" '{
      choiceArguments: {expectedAdmin: $dso,
        transfer: {sender: $sender, receiver: $receiver, amount: $amount, instrumentId: {admin: $dso, id: "Amulet"},
                   requestedAt: $now, executeBefore: $later, inputHoldingCids: $inputs, meta: {values: {}}},
        extraArgs: {context: {values: {}}, meta: {values: {}}}},
      excludeDebugFields: true}')"
}

# Registry disclosures in DisclosedContract form.
registry_disclosures() {
  jq -c '[.choiceContext.disclosedContracts[] | {templateId, contractId, createdEventBlob, synchronizerId}]' <<<"$1"
}

registry_extra_args() {
  jq -c '{context: .choiceContext.choiceContextData, meta: {values: {}}}' <<<"$1"
}

# amulet_holdings <validator> <party>: [{contractId, amount}] of the party's unlocked Amulets.
amulet_holdings() {
  active_contracts "$1" "$2" ":Splice.Amulet:Amulet" \
    | jq -c --arg owner "$2" '[.[] | select(.createArgument.owner == $owner)
        | {contractId, amount: .createArgument.amount.initialAmount}]'
}

# amulet_holdings_with_round <validator> <party>: [{contractId, amount, createdRound}] of the party's unlocked Amulets.
amulet_holdings_with_round() {
  active_contracts "$1" "$2" ":Splice.Amulet:Amulet" \
    | jq -c --arg owner "$2" '[.[] | select(.createArgument.owner == $owner)
        | {contractId, amount: .createArgument.amount.initialAmount, createdRound: (.createArgument.amount.createdAt.number | tonumber)}]'
}

# latest_open_round: the highest open mining round number that Scan reports.
latest_open_round() {
  curl --max-time 120 -sSf -X POST -H "$SCAN_HOST_HEADER" -H 'Content-Type: application/json' \
    "$SCAN_URL/api/scan/v0/open-and-issuing-mining-rounds" \
    -d '{"cached_open_mining_round_contract_ids": [], "cached_issuing_round_contract_ids": []}' \
    | jq -er '[.open_mining_rounds[].contract.payload.round.number | tonumber] | max'
}

# covering_replay_node <namespace> <digest-hex>: the active ReplayNode of guardianGovernance that covers the digest.
covering_replay_node() {
  active_contracts "$SUBMITTING_VALIDATOR" "$GG_PARTY" ":Wormhole.Core.Replay:ReplayNode" \
    | jq -er --arg gg "$GG_PARTY" --arg ns "$1" --arg digest "$2" \
      '[.[] | select(.createArgument.consumer == $gg and .createArgument.namespace == $ns)
             | select(.createArgument.prefix as $prefix | $digest | startswith($prefix))]
       | if length == 1 then .[0].contractId else error("expected one covering replay node, found \(length)") end'
}

# ntt_transfer_vaa <ntt-transfer flags...>: a VAA from the current guardian set.
ntt_transfer_vaa() {
  generate_vaa -kind ntt-transfer -signing-set-index "$NTT_GUARDIAN_SET_INDEX" \
    -signing-key-indices "$NTT_GUARDIAN_KEY_INDICES" "$@"
}

# allocate_external_party <validator> <party-hint> <key-file-prefix>: print {partyId, fingerprint, keyFile}.
allocate_external_party() {
  node "$LOCALNET_SCRIPT_DIR/allocate_external_party.js" "$(json_api_url "$1")" "$gg_work_dir/script.jwt" \
    "$LOGICAL_SYNCHRONIZER_ID" "$2" "$3"
}

# current_emitter <emitter-id>: the active Emitter of guardianGovernance with that id, as created event JSON.
current_emitter() {
  active_contracts "$SUBMITTING_VALIDATOR" "$GG_PARTY" ":Wormhole.Core.State:Emitter" \
    | jq -ec --arg gg "$GG_PARTY" --arg id "$1" '[.[] | select(.createArgument.owner == $gg and .createArgument.emitterId == $id)]
        | if length == 1 then .[0] else error("expected one active emitter \($id), found \(length)") end'
}

current_ntt_governance() {
  active_contracts "$SUBMITTING_VALIDATOR" "$GG_PARTY" ":Wormhole.Ntt.Governance:NttGovernance" \
    | jq -er --arg gg "$GG_PARTY" '[.[] | select(.createArgument.guardianGovernance == $gg)]
        | if length == 1 then .[0].contractId else error("expected one NttGovernance, found \(length)") end'
}

current_emitter_registry() {
  core_contract_id emitterRegistry
}


readonly DISCLOSURE_SERVICE_CONTAINER_UID=65532
# restrict_secret_to_service_user <file>: mode 0600. The disclosure service refuses a secret that other users can read.
# Docker Desktop maps a bind mount to the container user; on Linux the file must belong to the distroless nonroot user.
restrict_secret_to_service_user() {
  chmod 600 "$1"
  if [[ "$(uname -s)" == Linux ]]; then
    chown "$DISCLOSURE_SERVICE_CONTAINER_UID:$DISCLOSURE_SERVICE_CONTAINER_UID" "$1" \
      || fail "cannot give $1 to uid $DISCLOSURE_SERVICE_CONTAINER_UID; run the script as root"
  fi
}
