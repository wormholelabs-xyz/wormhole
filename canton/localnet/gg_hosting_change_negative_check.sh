#!/usr/bin/env bash
# Negative checks of the guardian replacement rehearsal (L1): a PartyToParticipant change that only one owner key signs,
# and a change that adds a host without the consent of that host, do not take effect.
# Usage: gg_hosting_change_negative_check.sh
set -euo pipefail
# shellcheck source=gg_lib.sh
source "$(dirname "${BASH_SOURCE[0]}")/gg_lib.sh"

readonly MAPPING_WAIT_ATTEMPTS=40
check_failures=0

# expect_no_effect <description> <phase> [VARIABLE=value...]: the Admin API rejects the replacement with
# TOPOLOGY_UNAUTHORIZED_TRANSACTION, or the replacement times out. The Admin API redacts the error code; the check finds
# it in the canton container log by the trace id.
expect_no_effect() {
  local description="$1" phase="$2" phase_output started_at trace_id rejection
  shift 2
  started_at="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  phase_output="$(env "$@" REPLICATION_AWAIT_MAX_ATTEMPTS="$MAPPING_WAIT_ATTEMPTS" \
    "$LOCALNET_SCRIPT_DIR/party_replication.sh" "$phase" 2>&1)" || true
  trace_id="$(grep -oE 'inquire about the request [0-9a-f]{32}' <<<"$phase_output" | head -n 1 | awk '{print $NF}')"
  rejection=""
  if [[ -n "$trace_id" ]]; then
    rejection="$(docker logs canton --since "$started_at" 2>&1 | grep "$trace_id" | grep -oE 'TOPOLOGY_UNAUTHORIZED_TRANSACTION[^"]{0,160}' | head -n 1)"
  fi
  if grep -q "^REPLICATION $phase done" <<<"$phase_output"; then
    echo "FAIL: $description took effect"
    check_failures=$((check_failures + 1))
  elif [[ -n "$rejection" ]]; then
    echo "PASS: $description rejected: $rejection"
  elif grep -q "topology not effective: PartyToParticipant of" <<<"$phase_output"; then
    echo "PASS: $description did not take effect"
  else
    echo "FAIL: $description stopped for another reason:"
    grep -E 'ERROR|REPLICATION|error' <<<"$phase_output" | head -n 5
    check_failures=$((check_failures + 1))
  fi
}

expect_no_effect "a threshold change that one owner key signs" set-threshold REPLICATION_SIGNER_COUNT=1 REPLICATION_CONFIRMATION_THRESHOLD=1
expect_no_effect "adding app-user as a host without the consent of app-user" add-host-without-consent REPLICATION_ADDED_HOST=app-user
"$LOCALNET_SCRIPT_DIR/gg_show_topology.sh" | grep PTP
[[ "$check_failures" -eq 0 ]] || fail "$check_failures negative check(s) failed"
echo "PASS: gg_hosting_change_negative_check"
