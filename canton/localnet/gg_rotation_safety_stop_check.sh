#!/usr/bin/env bash
# Safety stop rehearsal of gg_rotate_owner_key.sh. Each stop case asserts a non-zero exit, the SAFETY STOP text and
# an unchanged topology (namespace and PartyToParticipant serials of each participant).
#   SS1 GG_ROTATION_FAULT_PARTY_IDS names guardianGovernance only, while nttCustody exists. Stop: parties differ.
#   SS3 GG_ROTATION_FAULT_PARTY_IDS adds ghost::<namespace>. Stop: parties differ.
#   SS2 Half-applied key state: rotation A omits nttCustody from the load (ROTATION HALF-APPLIED). Rotation B then
#       starts from the owners after A, without --resume. Stop: nttCustody keys are neither the old nor the new set.
#   SS4 --resume of rotation A passes the safety stop and finishes A.
# Usage: gg_rotation_safety_stop_check.sh <removed-owner> <kept-owner> <kept-owner>
#   The current owner names, for example: owner3 owner1 owner2. New owner names are stop-<case>-<time>.
# Precondition: custody_host.sh and gg_package_vetting.sh ran.
# Output: the last line names the current owners after the run.
set -euo pipefail
# shellcheck source=gg_lib.sh
source "$(dirname "${BASH_SOURCE[0]}")/gg_lib.sh"

if [[ $# -ne 3 ]]; then
  echo "usage: gg_rotation_safety_stop_check.sh <removed-owner> <kept-owner> <kept-owner>" >&2
  exit 2
fi
check_owner_names_are_safe "$@"
check_owner_names_differ "$@"
if [[ -z "$CUSTODY_PARTY" ]]; then
  fail "missing $CUSTODY_ARTIFACTS. Run custody_host.sh first."
fi
owner_to_remove="$1" kept_owner_1="$2" kept_owner_2="$3"
RUN_SUFFIX="$(date +%s)"
readonly RUN_SUFFIX
readonly ROTATE="$LOCALNET_SCRIPT_DIR/gg_rotate_owner_key.sh"

topology_snapshot() {
  "$LOCALNET_SCRIPT_DIR/gg_show_topology.sh" | grep -E '^TOPOLOGY ' | sort > "$1"
  [[ -s "$1" ]] || fail "gg_show_topology.sh printed no TOPOLOGY lines"
}

# expect_safety_stop <case> <expected SAFETY STOP text> <env assignment>... -- <rotation arguments>...
expect_safety_stop() {
  local case_name="$1" expected_text="$2" rotation_status=0
  shift 2
  local -a environment_assignments=()
  while [[ "$1" != "--" ]]; do
    environment_assignments+=("$1")
    shift
  done
  shift
  topology_snapshot "$gg_work_dir/$case_name-before.topology"
  env ${environment_assignments[@]+"${environment_assignments[@]}"} "$ROTATE" "$@" > "$gg_work_dir/$case_name.log" 2>&1 \
    || rotation_status=$?
  grep -E '^(ROTATION|FAIL)' "$gg_work_dir/$case_name.log" | sed "s/^/  [$case_name] /" || true
  [[ "$rotation_status" -ne 0 ]] || fail "$case_name: the rotation exited 0"
  grep -qF -- "$expected_text" "$gg_work_dir/$case_name.log" || fail "$case_name: no line with: $expected_text"
  if grep -qE '^ROTATION (sign phase|load)' "$gg_work_dir/$case_name.log"; then
    fail "$case_name: the run reached the sign or load phase"
  fi
  topology_snapshot "$gg_work_dir/$case_name-after.topology"
  diff "$gg_work_dir/$case_name-before.topology" "$gg_work_dir/$case_name-after.topology" || fail "$case_name: the topology changed"
  echo "PASS $case_name: safety stop, topology unchanged"
}

echo "== SS1: rotation party list without nttCustody"
expect_safety_stop SS1 \
  "SAFETY STOP: PartyToParticipant parties in the namespace guardianGovernance,nttCustody differ from GG_ROTATION_PARTY_IDS guardianGovernance" \
  GG_ROTATION_FAULT_PARTY_IDS="$GG_PARTY" -- "$owner_to_remove" "$kept_owner_1" "$kept_owner_2" "stop-ss1-$RUN_SUFFIX"

echo "== SS3: rotation party list with an unknown party"
expect_safety_stop SS3 \
  "differ from GG_ROTATION_PARTY_IDS nttCustody,ghost,guardianGovernance" \
  GG_ROTATION_FAULT_PARTY_IDS="$CUSTODY_PARTY,ghost::${GG_PARTY#*::},$GG_PARTY" -- \
  "$owner_to_remove" "$kept_owner_1" "$kept_owner_2" "stop-ss3-$RUN_SUFFIX"

echo "== SS2: rotation B on the half-applied key state of rotation A"
rotation_a_new_owner="stop-ss2a-$RUN_SUFFIX"
rotation_a_status=0
GG_ROTATION_FAULT_OMIT_PARTY_OF=nttCustody "$ROTATE" "$owner_to_remove" "$kept_owner_1" "$kept_owner_2" "$rotation_a_new_owner" \
  > "$gg_work_dir/SS2-A.log" 2>&1 || rotation_a_status=$?
grep -E '^ROTATION' "$gg_work_dir/SS2-A.log" | sed 's/^/  [SS2-A] /' || true
[[ "$rotation_a_status" -ne 0 ]] || fail "SS2: rotation A exited 0"
grep -qF 'ROTATION HALF-APPLIED: still accept the removed key: nttCustody.' "$gg_work_dir/SS2-A.log" \
  || fail "SS2: rotation A is not half-applied for nttCustody only"
expect_safety_stop SS2 "SAFETY STOP: nttCustody signing keys" -- \
  "$rotation_a_new_owner" "$kept_owner_1" "$kept_owner_2" "stop-ss2b-$RUN_SUFFIX"
grep -qF 'are neither the old set nor the new set' "$gg_work_dir/SS2.log" || fail "SS2: the stop does not name the key sets"

echo "== SS4: --resume of rotation A"
resume_status=0
"$ROTATE" --resume "$owner_to_remove" "$kept_owner_1" "$kept_owner_2" "$rotation_a_new_owner" > "$gg_work_dir/SS4.log" 2>&1 \
  || resume_status=$?
grep -E '^ROTATION' "$gg_work_dir/SS4.log" | sed 's/^/  [SS4] /' || true
[[ "$resume_status" -eq 0 ]] || fail "SS4: the resume exited $resume_status"
grep -qxF 'ROTATION safety stop passed' "$gg_work_dir/SS4.log" || fail "SS4: no 'ROTATION safety stop passed'"
grep -qxF 'ROTATION done' "$gg_work_dir/SS4.log" || fail "SS4: no 'ROTATION done'"
echo "PASS SS4: resume passed the safety stop and finished rotation A"

echo "PASS: safety stop cases SS1, SS2, SS3, SS4"
echo "Current owners: $kept_owner_1 $kept_owner_2 $rotation_a_new_owner"
