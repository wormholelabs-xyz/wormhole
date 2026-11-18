#!/usr/bin/env bash
# A8 rehearsal: guardianGovernance is hosted on 3 participants with confirmation threshold 2.
#   1. Disconnect sv and app-user, so only app-provider can confirm for guardianGovernance.
#      a. SubmitGovernanceVAA (consumes the gg-signed CoreState). Expect MEDIATOR_SAYS_TX_TIMED_OUT.
#      b. ParseAndVerifyVAA (nonconsuming; gg signs the used CoreState). Expect MEDIATOR_SAYS_TX_TIMED_OUT.
#   2. Reconnect app-user. Repeat 1a. Expect commit with two confirming hosts.
#   3. Disconnect app-user again. PublishMessage at fee 0. Expect MEDIATOR_SAYS_TX_TIMED_OUT.
# Each timeout rejection must name guardianGovernance, and only it, as the unresponsive party.
# The mediator timeout runs on sequencer time. With two hosts offline, LocalNet traffic is too low to
# move it, so a background ticker requests time proofs.
# Usage: gg_confirmation_quorum.sh [<current-set-key-indices>]
#   Comma-separated devnet key indices of the current CoreState guardian set (default 0). After
#   gg_guardian_set_rotation.sh 0 1, pass 1.
# Usage: gg_confirmation_quorum.sh --party <party-id> [--signing-owners <owner>,<owner>]
#   Another party of the guardian namespace, for example nttCustody. Each case creates GovernanceCeremonyProbe as
#   that party, signed by the two owners (default owner1,owner2):
#     party-one-host-offline   app-user disconnected. Expect commit with two confirming hosts.
#     party-two-hosts-offline  sv and app-user disconnected. Expect MEDIATOR_SAYS_TX_TIMED_OUT, and the party is
#                              the only unresponsive party.
# Precondition: gg_publish_and_govern.sh ran (--party: custody_host.sh and gg_package_vetting.sh ran).
# The EXIT trap reconnects all participants.
set -euo pipefail
# shellcheck source=gg_lib.sh
source "$(dirname "${BASH_SOURCE[0]}")/gg_lib.sh"

readonly QUORUM_TICKER_SECONDS=600
quorum_party="$GG_PARTY"
quorum_signing_owners=(owner1 owner2)
while [[ $# -gt 0 && "$1" == --* ]]; do
  case "$1" in
    --party) quorum_party="${2:?--party needs a party id}"; shift 2 ;;
    --signing-owners) IFS=, read -r -a quorum_signing_owners <<<"${2:?--signing-owners needs two owner names}"; shift 2 ;;
    *) echo "unknown option $1" >&2; exit 2 ;;
  esac
done
if [[ "${quorum_party#*::}" != "${GG_PARTY#*::}" ]]; then
  echo "$quorum_party is outside the namespace of $GG_PARTY" >&2
  exit 2
fi
if [[ ${#quorum_signing_owners[@]} -ne 2 ]]; then
  echo "--signing-owners needs exactly two owner names" >&2
  exit 2
fi
check_owner_names_are_safe "${quorum_signing_owners[@]}"
check_owner_names_differ "${quorum_signing_owners[@]}"
current_set_key_indices="${1:-0}"
if [[ ! "$current_set_key_indices" =~ ^[0-9]+(,[0-9]+)*$ ]]; then
  echo "key indices must be comma-separated integers: $current_set_key_indices" >&2
  exit 2
fi

reconnect_all() {
  local exit_status=$?
  stop_clock_ticker
  echo "Reconnect sv and app-user"
  if ! set_participant_connection reconnect sv,app-user; then
    echo "ERROR: a participant stays disconnected from the global synchronizer. LocalNet is degraded." >&2
    echo "ERROR: recover with: PARTICIPANT_ACTION=reconnect PARTICIPANT_NAMES=sv,app-user $LOCALNET_SCRIPT_DIR/canton_console.sh $LOCALNET_SCRIPT_DIR/console/participant_connection.canton" >&2
    exit_status=1
  fi
  rm -rf "$gg_work_dir"
  exit "$exit_status"
}
trap reconnect_all EXIT

# check_timeout_rejection <output-prefix> [<party-id>]: <prefix>.out must hold a completion rejected with
# MEDIATOR_SAYS_TX_TIMED_OUT whose unresponsive parties are exactly that party (default guardianGovernance).
check_timeout_rejection() {
  local expected_party="${2:-$GG_PARTY}" summary
  summary="$(completion_summary "$1.out" 2>/dev/null || echo '{}')"
  if ! jq -e '.message | test("^MEDIATOR_SAYS_TX_TIMED_OUT")' <<<"$summary" >/dev/null 2>&1; then
    echo "FAIL: expected MEDIATOR_SAYS_TX_TIMED_OUT" >&2
    cat "$1.out" "$1.err" >&2
    exit 1
  fi
  if ! jq -e --arg party "$expected_party" '(.unresponsiveParties // "") | split(",") | map(select(length > 0)) == [$party]' \
      <<<"$summary" >/dev/null 2>&1; then
    echo "FAIL: expected unresponsiveParties to be exactly $expected_party" >&2
    cat "$1.out" "$1.err" >&2
    exit 1
  fi
  echo "PASS: rejected. $summary"
}

# run_party_quorum_case <case> <comma-separated disconnected hosts> <expected: commit or timeout>
run_party_quorum_case() {
  local case_name="$1" disconnected_hosts="$2" expected="$3" summary
  echo "Case $case_name: disconnect $disconnected_hosts, ${quorum_party%%::*} creates a probe. Expect $expected"
  set_participant_connection disconnect "$disconnected_hosts"
  SUBMIT_AWAIT_COMPLETION=true create_ceremony_probe "$quorum_party" "quorum-$case_name" "${quorum_signing_owners[@]}" \
    > "$gg_work_dir/$case_name.out" 2> "$gg_work_dir/$case_name.err" || true
  case "$expected" in
    commit)
      summary="$(completion_summary "$gg_work_dir/$case_name.out" 2>/dev/null || echo '{}')"
      if ! jq -e '.code == 0' <<<"$summary" >/dev/null 2>&1; then
        echo "FAIL: expected commit. $summary" >&2
        cat "$gg_work_dir/$case_name.err" >&2
        exit 1
      fi
      echo "PASS: committed. $summary"
      ;;
    timeout) check_timeout_rejection "$gg_work_dir/$case_name" "$quorum_party" ;;
  esac
  set_participant_connection reconnect "$disconnected_hosts"
}

if [[ "$quorum_party" != "$GG_PARTY" ]]; then
  start_clock_ticker "$QUORUM_TICKER_SECONDS"
  run_party_quorum_case party-one-host-offline app-user commit
  run_party_quorum_case party-two-hosts-offline sv,app-user timeout
  echo "PASS: confirmation threshold 2 of 3 for ${quorum_party%%::*}"
  exit 0
fi

current_set_index="$(active_contracts "$SUBMITTING_VALIDATOR" "$OPERATOR_PARTY" :Wormhole.Core.State:CoreState \
  | jq -er 'if length == 1 then .[0].createArgument.guardianSetIndex else error("expected one CoreState") end')"
echo "Current guardian set $current_set_index, signing keys $current_set_key_indices"
# Governance replay protection is keyed by digest, so each run uses a new sequence.
fee_zero_vaa="$(generate_vaa -kind set-message-fee -fee 0 -signing-set-index "$current_set_index" \
  -signing-key-indices "$current_set_key_indices" -sequence "$(date +%s)")"
fee_zero_vaa_hex="$(jq -er .vaaHex <<<"$fee_zero_vaa")"
set_zero_keys="$(vaa_pub_keys_json "$fee_zero_vaa")"
read_only_vaa="$(generate_vaa -kind message -signing-set-index "$current_set_index" \
  -signing-key-indices "$current_set_key_indices" -sequence 1)"

echo "Step 1: disconnect sv and app-user"
set_participant_connection disconnect sv,app-user
start_clock_ticker "$QUORUM_TICKER_SECONDS"

echo "Step 1a: SubmitGovernanceVAA with one confirming host"
if SUBMIT_AWAIT_COMPLETION=true submit_governance_vaa "quorum-governance-one-host-$(date +%s)" \
    "$fee_zero_vaa_hex" "$set_zero_keys" > "$gg_work_dir/1a.out" 2> "$gg_work_dir/1a.err"; then
  echo "FAIL: the governance transaction committed with one confirming host" >&2
  exit 1
fi
check_timeout_rejection "$gg_work_dir/1a"

echo "Step 1b: ParseAndVerifyVAA with one confirming host"
SUBMIT_AWAIT_COMPLETION=true verify_vaa_as_operator "quorum-read-one-host-$(date +%s)" \
  "$(jq -er .vaaHex <<<"$read_only_vaa")" "$(vaa_pub_keys_json "$read_only_vaa")" > "$gg_work_dir/1b.out" 2> "$gg_work_dir/1b.err" || true
check_timeout_rejection "$gg_work_dir/1b"

echo "Step 2: reconnect app-user (two confirming hosts)"
set_participant_connection reconnect app-user
governance_transaction="$(submit_governance_vaa "quorum-governance-two-hosts-$(date +%s)" "$fee_zero_vaa_hex" "$set_zero_keys")"
created_contract_id "$governance_transaction" ":Wormhole.Core.State:CoreState" >/dev/null
echo "PASS: committed with two confirming hosts, update $(jq -r .updateId <<<"$governance_transaction"). messageFee is 0 again."

echo "Step 3: disconnect app-user. PublishMessage with one confirming host"
set_participant_connection disconnect app-user
SUBMIT_AWAIT_COMPLETION=true publish_message "quorum-publish-one-host-$(date +%s)" "6f6e6520686f7374" \
  > "$gg_work_dir/3.out" 2> "$gg_work_dir/3.err" || true
check_timeout_rejection "$gg_work_dir/3"
