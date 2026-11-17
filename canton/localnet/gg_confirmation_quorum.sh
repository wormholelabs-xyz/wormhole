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
# Usage: gg_confirmation_quorum.sh
# Precondition: gg_publish_and_govern.sh ran. The EXIT trap reconnects all participants.
set -euo pipefail
# shellcheck source=gg_lib.sh
source "$(dirname "${BASH_SOURCE[0]}")/gg_lib.sh"

readonly QUORUM_TICKER_SECONDS=600

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

# check_timeout_rejection <output-prefix>: <prefix>.out must hold a completion rejected with
# MEDIATOR_SAYS_TX_TIMED_OUT whose unresponsive parties are exactly guardianGovernance.
check_timeout_rejection() {
  local summary
  summary="$(completion_summary "$1.out" 2>/dev/null || echo '{}')"
  if ! jq -e '.message | test("^MEDIATOR_SAYS_TX_TIMED_OUT")' <<<"$summary" >/dev/null 2>&1; then
    echo "FAIL: expected MEDIATOR_SAYS_TX_TIMED_OUT" >&2
    cat "$1.out" "$1.err" >&2
    exit 1
  fi
  if ! jq -e --arg gg "$GG_PARTY" '(.unresponsiveParties // "") | split(",") | map(select(length > 0)) == [$gg]' \
      <<<"$summary" >/dev/null 2>&1; then
    echo "FAIL: expected unresponsiveParties to be exactly $GG_PARTY" >&2
    cat "$1.out" "$1.err" >&2
    exit 1
  fi
  echo "PASS: rejected. $summary"
}

# Governance replay protection is keyed by digest, so each run uses a new sequence.
fee_zero_vaa="$(generate_vaa -kind set-message-fee -fee 0 -signing-set-index 0 -signing-key-indices 0 -sequence "$(date +%s)")"
fee_zero_vaa_hex="$(jq -r .vaaHex <<<"$fee_zero_vaa")"
set_zero_keys="$(vaa_pub_keys_json "$fee_zero_vaa")"
read_only_vaa="$(generate_vaa -kind message -signing-set-index 0 -signing-key-indices 0 -sequence 1)"

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
  "$(jq -r .vaaHex <<<"$read_only_vaa")" "$(vaa_pub_keys_json "$read_only_vaa")" > "$gg_work_dir/1b.out" 2> "$gg_work_dir/1b.err" || true
check_timeout_rejection "$gg_work_dir/1b"

echo "Step 2: reconnect app-user (two confirming hosts)"
set_participant_connection reconnect app-user
governance_transaction="$(submit_governance_vaa "quorum-governance-two-hosts-$(date +%s)" "$fee_zero_vaa_hex" "$set_zero_keys")"
record_core_contract_id coreState "$(created_contract_id "$governance_transaction" ":Wormhole.Core.State:CoreState")"
echo "PASS: committed with two confirming hosts, update $(jq -r .updateId <<<"$governance_transaction"). messageFee is 0 again."

echo "Step 3: disconnect app-user. PublishMessage with one confirming host"
set_participant_connection disconnect app-user
SUBMIT_AWAIT_COMPLETION=true publish_message "quorum-publish-one-host-$(date +%s)" "6f6e6520686f7374" \
  > "$gg_work_dir/3.out" 2> "$gg_work_dir/3.err" || true
check_timeout_rejection "$gg_work_dir/3"
