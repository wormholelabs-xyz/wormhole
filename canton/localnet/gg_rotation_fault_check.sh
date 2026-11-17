#!/usr/bin/env bash
# Rotation fault rehearsal of gg_rotate_owner_key.sh with nttCustody and guardianGovernance in the namespace.
# The cases run in this order. Each case after R2 starts from the owners that the case before it left.
#   R2  Signer fault of the added owner. Expect ROTATION NOT STARTED and no topology change.
#   R1  Full rotation. Expect the party order nttCustody,guardianGovernance, protocol version 37, one effective time
#       for the 3 mappings, ROTATION done, and the ceremony checks of each party.
#   R3  nttCustody omitted from the load. Expect ROTATION HALF-APPLIED that names nttCustody only.
#   R6  Ceremony probes in the R3 state, asserted:
#         nttCustody:         kept + removed commits; kept + new is rejected (1 valid, 1 invalid).
#         guardianGovernance: kept + new commits; kept + removed is rejected (1 valid, 1 invalid).
#   R3b --resume of R3 with a signer fault of the added owner. Expect ROTATION done and 0 rotation signatures.
#   R4  Measurement: the nttCustody change in the load lacks the signature of the added owner.
#       Prints MEASURED R4 with the outcome. A half-applied outcome is completed with --resume.
#   R5  Measurement: the root delegation of the added owner goes into the same load call.
#       Prints MEASURED R5 with the outcome. A half-applied outcome is completed with --resume.
# Usage: gg_rotation_fault_check.sh <removed-owner> <kept-owner> <kept-owner>
#   The current owner names, for example: owner3 owner1 owner2. New owner names are rot-<case>-<time>.
# Precondition: custody_host.sh, gg_package_vetting.sh and custody_authority_check.sh ran.
# Output: the last line names the current owners after the run.
set -euo pipefail
# shellcheck source=gg_lib.sh
source "$(dirname "${BASH_SOURCE[0]}")/gg_lib.sh"

if [[ $# -ne 3 ]]; then
  echo "usage: gg_rotation_fault_check.sh <removed-owner> <kept-owner> <kept-owner>" >&2
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
readonly HALF_APPLIED_REJECTION_TEXT="valid signatures from distinct keys (1 invalid), but expected at least 2 valid for"

# topology_snapshot <file>: the namespace and PartyToParticipant lines of each participant, sorted.
topology_snapshot() {
  "$LOCALNET_SCRIPT_DIR/gg_show_topology.sh" | grep -E '^TOPOLOGY ' | sort > "$1"
  [[ -s "$1" ]] || fail "gg_show_topology.sh printed no TOPOLOGY lines"
}

# run_rotation <case> <env assignment>... -- <rotation arguments>...: runs gg_rotate_owner_key.sh with the
# assignments. Writes <case>.log and <case>.status in the work directory.
run_rotation() {
  local case_name="$1" rotation_status=0
  shift
  local -a environment_assignments=()
  while [[ "$1" != "--" ]]; do
    environment_assignments+=("$1")
    shift
  done
  shift
  env ${environment_assignments[@]+"${environment_assignments[@]}"} "$ROTATE" "$@" > "$gg_work_dir/$case_name.log" 2>&1 \
    || rotation_status=$?
  echo "$rotation_status" > "$gg_work_dir/$case_name.status"
  grep -E '^(ROTATION|PASS|FAIL)' "$gg_work_dir/$case_name.log" | sed "s/^/  [$case_name] /" || true
}

# expect_log_line <case> <fixed text>: the rotation log of that case has a line with the text.
expect_log_line() {
  grep -qF -- "$2" "$gg_work_dir/$1.log" || fail "$1: no line with: $2"
}

# rotation_outcome <case>: done, not-started, not-applied, half-applied or failed.
rotation_outcome() {
  local log_file="$gg_work_dir/$1.log"
  if grep -q '^ROTATION done' "$log_file"; then echo "done"
  elif grep -q '^ROTATION NOT STARTED' "$log_file"; then echo not-started
  elif grep -q '^ROTATION NOT APPLIED' "$log_file"; then echo not-applied
  elif grep -q '^ROTATION HALF-APPLIED' "$log_file"; then echo half-applied
  else echo failed
  fi
}

# expect_probe <party-id> <expected: commit or rejected> <owner> <owner>: ceremony probe of that party.
expect_probe() {
  local party_id="$1" expected="$2" probe_status=0
  shift 2
  create_ceremony_probe "$party_id" "fault-check-$(IFS=-; echo "$*")" "$@" > "$gg_work_dir/probe.out" 2> "$gg_work_dir/probe.err" \
    || probe_status=$?
  case "$expected" in
    commit)
      [[ "$probe_status" -eq 0 ]] || fail "R6: ${party_id%%::*} with $*: expected commit. $(head -c 600 "$gg_work_dir/probe.err")"
      ;;
    rejected)
      [[ "$probe_status" -ne 0 ]] || fail "R6: ${party_id%%::*} with $*: expected rejection, but committed"
      grep -qF 'FAILED_TO_EXECUTE_TRANSACTION' "$gg_work_dir/probe.err" || fail "R6: ${party_id%%::*} with $*: no FAILED_TO_EXECUTE_TRANSACTION"
      grep -qF "Received 1 $HALF_APPLIED_REJECTION_TEXT ${party_id%%::*}::" "$gg_work_dir/probe.err" \
        || fail "R6: ${party_id%%::*} with $*: expected 'Received 1 $HALF_APPLIED_REJECTION_TEXT ${party_id%%::*}::'"
      ;;
  esac
  echo "PASS R6: ${party_id%%::*} signed by $*: $expected"
}

# complete_measured_rotation <case> <removed> <new>: after a measurement, finish a half-applied rotation with
# --resume. Prints the owner that is current after the case (new owner, or removed owner when nothing applied).
complete_measured_rotation() {
  local case_name="$1" case_removed_owner="$2" case_new_owner="$3" outcome
  outcome="$(rotation_outcome "$case_name")"
  echo "MEASURED $case_name: outcome $outcome" >&2
  grep -E '^ROTATION (load attempt|effective time|NOT APPLIED|HALF-APPLIED)' "$gg_work_dir/$case_name.log" \
    | sed "s/^/MEASURED $case_name: /" >&2 || true
  case "$outcome" in
    "done") echo "$case_new_owner" ;;
    not-applied) echo "$case_removed_owner" ;;
    half-applied)
      run_rotation "$case_name-resume" -- --resume "$case_removed_owner" "$kept_owner_1" "$kept_owner_2" "$case_new_owner" >&2
      [[ "$(rotation_outcome "$case_name-resume")" == "done" ]] || fail "$case_name: --resume did not finish the rotation"
      echo "$case_new_owner"
      ;;
    *) fail "$case_name: unexpected outcome $outcome" ;;
  esac
}

echo "== R2: signer fault of the added owner"
r2_new_owner="rot-r2-$RUN_SUFFIX"
topology_snapshot "$gg_work_dir/r2-before.topology"
run_rotation R2 GG_ROTATION_FAULT_SIGNER_OF="$r2_new_owner" -- "$owner_to_remove" "$kept_owner_1" "$kept_owner_2" "$r2_new_owner"
[[ "$(cat "$gg_work_dir/R2.status")" -ne 0 ]] || fail "R2: the rotation exited 0"
[[ "$(rotation_outcome R2)" == not-started ]] || fail "R2: expected ROTATION NOT STARTED"
expect_log_line R2 "ROTATION FAULT INJECTION ACTIVE: GG_ROTATION_FAULT_SIGNER_OF=$r2_new_owner"
expect_log_line R2 "ROTATION before: added owner root delegation=absent"
topology_snapshot "$gg_work_dir/r2-after.topology"
diff "$gg_work_dir/r2-before.topology" "$gg_work_dir/r2-after.topology" || fail "R2: the topology changed"
echo "PASS R2: not started, topology unchanged"

echo "== R1: full rotation"
r1_new_owner="rot-r1-$RUN_SUFFIX"
run_rotation R1 -- "$owner_to_remove" "$kept_owner_1" "$kept_owner_2" "$r1_new_owner"
[[ "$(cat "$gg_work_dir/R1.status")" -eq 0 ]] || fail "R1: the rotation exited $(cat "$gg_work_dir/R1.status")"
expect_log_line R1 "ROTATION parties: nttCustody,guardianGovernance"
expect_log_line R1 "ROTATION protocol version: 37"
expect_log_line R1 "(same for all 3 mappings)"
expect_log_line R1 "ROTATION done"
echo "PASS R1: rotated $owner_to_remove to $r1_new_owner"
owner_to_remove="$r1_new_owner"

echo "== R3: nttCustody omitted from the load"
r3_new_owner="rot-r3-$RUN_SUFFIX"
topology_snapshot "$gg_work_dir/r3-before.topology"
run_rotation R3 GG_ROTATION_FAULT_OMIT_PARTY_OF=nttCustody -- "$owner_to_remove" "$kept_owner_1" "$kept_owner_2" "$r3_new_owner"
[[ "$(cat "$gg_work_dir/R3.status")" -ne 0 ]] || fail "R3: the rotation exited 0"
[[ "$(rotation_outcome R3)" == half-applied ]] || fail "R3: expected ROTATION HALF-APPLIED"
expect_log_line R3 "ROTATION HALF-APPLIED: still accept the removed key: nttCustody."
topology_snapshot "$gg_work_dir/r3-after.topology"
echo "MEASURED R3: changed topology lines:"
diff "$gg_work_dir/r3-before.topology" "$gg_work_dir/r3-after.topology" | sed 's/^/MEASURED R3: /' || true
echo "PASS R3: half-applied, nttCustody keeps the removed key"

echo "== R6: ceremony probes in the half-applied state"
expect_probe "$CUSTODY_PARTY" commit "$kept_owner_1" "$owner_to_remove"
expect_probe "$CUSTODY_PARTY" rejected "$kept_owner_1" "$r3_new_owner"
expect_probe "$GG_PARTY" commit "$kept_owner_1" "$r3_new_owner"
expect_probe "$GG_PARTY" rejected "$kept_owner_1" "$owner_to_remove"

echo "== R3b: --resume without a signature of the added owner"
run_rotation R3b GG_ROTATION_FAULT_SIGNER_OF="$r3_new_owner" -- --resume "$owner_to_remove" "$kept_owner_1" "$kept_owner_2" "$r3_new_owner"
[[ "$(cat "$gg_work_dir/R3b.status")" -eq 0 ]] || fail "R3b: the resume exited $(cat "$gg_work_dir/R3b.status")"
expect_log_line R3b "ROTATION sign phase skipped: stored transactions"
expect_log_line R3b "ROTATION signatures before the removed-owner check: 0"
expect_log_line R3b "ROTATION done"
echo "PASS R3b: resumed without signers"
owner_to_remove="$r3_new_owner"

echo "== R4: measurement, nttCustody change without the signature of the added owner"
r4_new_owner="rot-r4-$RUN_SUFFIX"
run_rotation R4 GG_ROTATION_FAULT_UNSIGNED_BY_ADDED_OF=nttCustody -- "$owner_to_remove" "$kept_owner_1" "$kept_owner_2" "$r4_new_owner"
owner_to_remove="$(complete_measured_rotation R4 "$owner_to_remove" "$r4_new_owner")"

echo "== R5: measurement, root delegation in the same load call"
r5_new_owner="rot-r5-$RUN_SUFFIX"
run_rotation R5 GG_ROTATION_MEASURE_SINGLE_LOAD=true -- "$owner_to_remove" "$kept_owner_1" "$kept_owner_2" "$r5_new_owner"
owner_to_remove="$(complete_measured_rotation R5 "$owner_to_remove" "$r5_new_owner")"

echo "PASS: rotation fault cases R1, R2, R3, R3b, R6; measured R3, R4, R5"
echo "Current owners: $kept_owner_1 $kept_owner_2 $owner_to_remove"
