#!/usr/bin/env bash
# Create the NttGovernance root (signatories Operator and guardianGovernance, 2-of-3 owner signatures)
# and the canonical CoinFactory (admin guardianGovernance) for burn/mint deployments.
# Usage: ntt_governance.sh
# Precondition: gg_genesis.sh and ntt_upload_dars.sh ran. Output: $LOCALNET_STATE_DIR/ntt-contracts.json.
# A second run keeps the existing root and factory: guardians sign at most one root per operator.
set -euo pipefail
# shellcheck source=ntt_lib.sh
source "$(dirname "${BASH_SOURCE[0]}")/ntt_lib.sh"

operator_and_gg="$(jq -n --arg op "$OPERATOR_PARTY" --arg gg "$GG_PARTY" '[$op, $gg]')"

existing_roots="$(active_contracts "$SUBMITTING_VALIDATOR" "$OPERATOR_PARTY" ":Wormhole.Ntt.Governance:NttGovernance" \
  | jq --arg gg "$GG_PARTY" '[.[] | select(.createArgument.guardianGovernance == $gg)]')"
case "$(jq length <<<"$existing_roots")" in
  0)
    echo "Create NttGovernance: Operator + guardianGovernance ${NTT_GG_SIGNING_OWNERS[*]}"
    root_transaction="$(submit_interactive "ntt-governance-root-$(date +%s)" "$operator_and_gg" \
      "$(create_command "$NTT_GOVERNANCE_TEMPLATE" "$(jq -n --arg op "$OPERATOR_PARTY" --arg gg "$GG_PARTY" \
        '{operator: $op, guardianGovernance: $gg, nextId: "0", consumedGovernance: {map: []}}')")" \
      "$(jq -s 'add' <(operator_signers_json) <(gg_ceremony_signers_json))")"
    root_cid="$(created_contract_id "$root_transaction" ":Wormhole.Ntt.Governance:NttGovernance")"
    echo "NttGovernance $root_cid (update $(jq -r .updateId <<<"$root_transaction"))"
    ;;
  1)
    root_cid="$(jq -r '.[0].contractId' <<<"$existing_roots")"
    echo "NttGovernance exists: $root_cid (nextId $(jq -r '.[0].createArgument.nextId' <<<"$existing_roots"))"
    ;;
  *)
    echo "Found $(jq length <<<"$existing_roots") NttGovernance roots for guardianGovernance; expected at most one" >&2
    exit 1
    ;;
esac
record_ntt_contract nttGovernance "$root_cid"

existing_factories="$(active_contracts "$SUBMITTING_VALIDATOR" "$GG_PARTY" ":Token.CIP0056.CoinFactory:CoinFactory" \
  | jq --arg gg "$GG_PARTY" '[.[] | select(.createArgument.admin == $gg)]')"
case "$(jq length <<<"$existing_factories")" in
  0)
    echo "Create CoinFactory: guardianGovernance ${NTT_GG_SIGNING_OWNERS[*]}"
    factory_transaction="$(submit_interactive "ntt-coin-factory-$(date +%s)" "$(jq -n --arg gg "$GG_PARTY" '[$gg]')" \
      "$(create_command "$COIN_FACTORY_TEMPLATE" "$(jq -n --arg gg "$GG_PARTY" '{admin: $gg}')")" \
      "$(gg_ceremony_signers_json)")"
    factory_cid="$(created_contract_id "$factory_transaction" ":Token.CIP0056.CoinFactory:CoinFactory")"
    echo "CoinFactory $factory_cid (update $(jq -er .updateId <<<"$factory_transaction"))"
    ;;
  1)
    factory_cid="$(jq -er '.[0].contractId' <<<"$existing_factories")"
    echo "CoinFactory exists: $factory_cid"
    ;;
  *)
    echo "Found $(jq length <<<"$existing_factories") CoinFactory contracts with admin guardianGovernance; expected at most one" >&2
    exit 1
    ;;
esac
record_ntt_contract coinFactory "$factory_cid"

for validator_name in app-provider sv app-user; do
  echo "$validator_name sees for guardianGovernance: NttGovernance $(active_contracts "$validator_name" "$GG_PARTY" :Wormhole.Ntt.Governance:NttGovernance | jq length), CoinFactory $(active_contracts "$validator_name" "$GG_PARTY" :Token.CIP0056.CoinFactory:CoinFactory | jq length)"
done
