#!/usr/bin/env bash
# Negative checks of the guardian replacement rehearsal (L1): a PartyToParticipant change that only one owner key signs,
# a change that adds a host without the consent of that host, and an Observation to Confirmation promotion of guardian4
# without the consent of guardian4 do not take effect.
# Precondition: guardian4 hosts both parties at Confirmation. The promotion case sets guardian4 to Observation first;
# the EXIT trap restores Confirmation.
# Usage: gg_hosting_change_negative_check.sh [owner-name owner-name]
set -euo pipefail
# shellcheck source=gg_lib.sh
source "$(dirname "${BASH_SOURCE[0]}")/gg_lib.sh"

readonly MAPPING_WAIT_ATTEMPTS=40
first_owner="${1:-owner1}"
second_owner="${2:-owner2}"
check_owner_names_are_safe "$first_owner" "$second_owner"
check_owner_names_differ "$first_owner" "$second_owner"
check_failures=0

# expect_no_effect <description> <phase> [VARIABLE=value...]: the Admin API rejects the replacement with
# TOPOLOGY_UNAUTHORIZED_TRANSACTION, or the replacement times out. The Admin API redacts the error code; the check finds
# it in the canton container log by the trace id.
expect_no_effect() {
  local description="$1" phase="$2" phase_output started_at trace_id rejection
  shift 2
  started_at="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  phase_output="$(env "$@" REPLICATION_AWAIT_MAX_ATTEMPTS="$MAPPING_WAIT_ATTEMPTS" \
    "$LOCALNET_SCRIPT_DIR/party_replication.sh" "$phase" "$first_owner" "$second_owner" 2>&1)" || true
  trace_id="$(grep -m1 -oE 'inquire about the request [0-9a-f]{32}' <<<"$phase_output" | awk '{print $NF}' || true)"
  rejection=""
  if [[ -n "$trace_id" ]]; then
    rejection="$(docker logs canton --since "$started_at" 2>&1 | grep -F "$trace_id" \
      | grep -m1 -oE 'TOPOLOGY_UNAUTHORIZED_TRANSACTION[^"]{0,160}' || true)"
  fi
  if grep -q "^REPLICATION $phase done" <<<"$phase_output"; then
    echo "FAIL: $description took effect"
    check_failures=$((check_failures + 1))
  elif [[ -n "$rejection" ]]; then
    echo "PASS: $description rejected: $rejection"
  elif grep -q "topology not effective: PartyToParticipant of" <<<"$phase_output"; then
    echo "PASS: $description did not take effect (no rejection code for trace id ${trace_id:-none})"
  else
    echo "FAIL: $description stopped for another reason:"
    grep -E 'ERROR|REPLICATION|error' <<<"$phase_output" | head -n 5 || true
    check_failures=$((check_failures + 1))
  fi
}

# set_guardian4_permission <Observation|Confirmation>: set-permission with the consent of guardian4.
set_guardian4_permission() {
  REPLICATION_PERMISSION_HOST=guardian4 REPLICATION_HOST_PERMISSION="$1" \
    "$LOCALNET_SCRIPT_DIR/party_replication.sh" set-permission "$first_owner" "$second_owner" > "$gg_work_dir/set-permission.log" 2>&1 \
    || { cat "$gg_work_dir/set-permission.log" >&2; return 1; }
  echo "guardian4 permission set to $1"
}

restore_guardian4_confirmation() {
  local exit_status=$?
  if ! set_guardian4_permission Confirmation; then
    echo "ERROR: guardian4 is not at Confirmation; run party_replication.sh set-permission" >&2
    exit_status=1
  fi
  rm -rf "$gg_work_dir"
  exit "$exit_status"
}

expect_no_effect "a threshold change that one owner key signs" set-threshold REPLICATION_SIGNER_COUNT=1 REPLICATION_CONFIRMATION_THRESHOLD=1
expect_no_effect "adding app-user as a host without the consent of app-user" add-host-without-consent REPLICATION_ADDED_HOST=app-user
trap restore_guardian4_confirmation EXIT
set_guardian4_permission Observation || fail "cannot set guardian4 to Observation"
expect_no_effect "promoting guardian4 from Observation to Confirmation without the consent of guardian4" set-permission-without-consent \
  REPLICATION_PERMISSION_HOST=guardian4 REPLICATION_HOST_PERMISSION=Confirmation
"$LOCALNET_SCRIPT_DIR/gg_show_topology.sh" | grep PTP || true
[[ "$check_failures" -eq 0 ]] || fail "$check_failures negative check(s) failed"
echo "PASS: gg_hosting_change_negative_check"
