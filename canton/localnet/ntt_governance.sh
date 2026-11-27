#!/usr/bin/env bash
# Record the NttGovernance root and the canonical guardianGovernance CoinFactory that AcceptGenesis created
# (gg_genesis.sh). Stops unless guardianGovernance sees exactly one of each, with the expected signatories.
# Usage: ntt_governance.sh
# Precondition: gg_genesis.sh and ntt_upload_dars.sh ran. Output: $LOCALNET_STATE_DIR/ntt-contracts.json.
set -euo pipefail
# shellcheck source=ntt_lib.sh
source "$(dirname "${BASH_SOURCE[0]}")/ntt_lib.sh"

root_event="$(active_contracts "$SUBMITTING_VALIDATOR" "$GG_PARTY" ":Wormhole.Ntt.Governance:NttGovernance" \
  | jq -ec --arg gg "$GG_PARTY" '[.[] | select(.createArgument.guardianGovernance == $gg)]
    | if length == 1 then .[0] else error("expected one NttGovernance of guardianGovernance, found \(length); run gg_genesis.sh") end')"
[[ "$(jq -c '.signatories | sort' <<<"$root_event")" == "$(jq -cn --arg gg "$GG_PARTY" --arg custody "$NTT_CUSTODY_PARTY" '[$gg, $custody] | sort')" ]] \
  || fail "NttGovernance signatories $(jq -c .signatories <<<"$root_event"), expected guardianGovernance and nttCustody"
root_cid="$(jq -r .contractId <<<"$root_event")"
root_identity="$(byte_variant_hex NttGovernanceRootIdentity "$(jq -c .createArgument.rootIdentity <<<"$root_event")")"
[[ "${#root_identity}" -eq 64 ]] || fail "NttGovernance rootIdentity has ${#root_identity} hex characters, expected 64"
echo "NttGovernance $root_cid (nextId $(jq -r .createArgument.nextId <<<"$root_event"), rootIdentity $root_identity)"
record_ntt_contract nttGovernance "$root_cid"
record_ntt_contract nttGovernanceRootIdentity "$root_identity"

factory_event="$(active_contracts "$SUBMITTING_VALIDATOR" "$GG_PARTY" ":Token.CIP0056.CoinFactory:CoinFactory" \
  | jq -ec --arg gg "$GG_PARTY" '[.[] | select(.createArgument.admin == $gg)]
    | if length == 1 then .[0] else error("expected one CoinFactory with admin guardianGovernance, found \(length); run gg_genesis.sh") end')"
[[ "$(jq -c .signatories <<<"$factory_event")" == "$(jq -cn --arg gg "$GG_PARTY" '[$gg]')" ]] \
  || fail "CoinFactory signatories $(jq -c .signatories <<<"$factory_event"), expected guardianGovernance"
factory_cid="$(jq -r .contractId <<<"$factory_event")"
echo "CoinFactory $factory_cid"
record_ntt_contract coinFactory "$factory_cid"

for validator_name in app-provider sv app-user; do
  echo "$validator_name sees for guardianGovernance: NttGovernance $(active_contracts "$validator_name" "$GG_PARTY" :Wormhole.Ntt.Governance:NttGovernance | jq length), CoinFactory $(active_contracts "$validator_name" "$GG_PARTY" :Token.CIP0056.CoinFactory:CoinFactory | jq length)"
done
