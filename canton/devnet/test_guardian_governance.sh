#!/usr/bin/env bash
#
# Local end-to-end test for the guardianGovernance external-party bootstrap.
# Starts (or reuses) a `dpm sandbox`, runs setup_guardian_governance.sh, then
# asserts:
#
#   - DN created: 3 owners, threshold 2.
#   - Party allocated: guardianGovernance::<dn>, hosted at Confirmation,
#     2-of-3 party signing keys.
#   - 1-of-3 co-signs: `CoreState` rejected (threshold enforced), and the
#     script stops before the `EmitterRegistry` create.
#   - 0-of-3 (operator alone): the same (anti-forgery -- the crux property).
#   - 2-of-3 co-signs `CoreState` alone (GENESIS_LAST_CREATE=CoreState): commits.
#   - A rerun skips the active `CoreState` and commits `EmitterRegistry`.
#   - A second rerun skips both.
#
# Usage: test_guardian_governance.sh
#   Assumes `dpm sandbox` is NOT already running on :6865/:6864 -- this script
#   starts one in a scratch directory and tears it down on exit. Set
#   GG_TEST_REUSE_SANDBOX=1 to instead reuse an already-running sandbox
#   (skips start/stop; useful for interactive debugging).
set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/../.." && pwd)"
WORK_DIR="$(mktemp -d /tmp/guardian-governance-test.XXXXXX)"
SANDBOX_PID=""
FAILURES=0

log() { echo "[test-guardian-governance] $*"; }
pass() { log "PASS: $*"; }
fail() { log "FAIL: $*"; FAILURES=$((FAILURES + 1)); }

# Max process-tree depth walked under the started `dpm` process.
MAX_SANDBOX_TREE_DEPTH=8

# Prints SANDBOX_PID and its descendants, parents first.
# shellcheck disable=SC2329 # invoked from cleanup (trap)
list_sandbox_process_tree() {
  local frontier="${SANDBOX_PID}" next_frontier pid
  for _ in $(seq 1 "${MAX_SANDBOX_TREE_DEPTH}"); do
    [ -n "${frontier}" ] || return 0
    echo "${frontier}"
    next_frontier=""
    for pid in ${frontier}; do
      next_frontier="${next_frontier} $(pgrep -P "${pid}" | tr '\n' ' ')"
    done
    frontier="$(echo "${next_frontier}" | xargs)"
  done
  [ -z "${frontier}" ] || log "WARNING: sandbox process tree deeper than ${MAX_SANDBOX_TREE_DEPTH}; not stopping: ${frontier}"
}

# shellcheck disable=SC2329 # invoked via trap
cleanup() {
  if [ -n "${SANDBOX_PID}" ]; then
    # `dpm sandbox` forks the JVM as a child; collect the tree before the
    # parent dies and the JVM is reparented to init.
    local sandbox_pids
    sandbox_pids="$(list_sandbox_process_tree | xargs)"
    log "stopping sandbox process tree (pids ${sandbox_pids})"
    # shellcheck disable=SC2086
    kill ${sandbox_pids} 2>/dev/null || true
    wait "${SANDBOX_PID}" 2>/dev/null || true
  fi
}
trap cleanup EXIT

if [ "${GG_TEST_REUSE_SANDBOX:-0}" != "1" ]; then
  if lsof -nP -iTCP:6865 -sTCP:LISTEN >/dev/null 2>&1; then
    log "port 6865 is already in use; stop that ledger or set GG_TEST_REUSE_SANDBOX=1"
    exit 1
  fi
  log "starting dpm sandbox in ${WORK_DIR}"
  (cd "${WORK_DIR}" && exec dpm sandbox --no-tty -C canton.participants.sandbox.ledger-api.address=0.0.0.0) \
    > "${WORK_DIR}/sandbox.log" 2>&1 &
  SANDBOX_PID=$!
fi

"${SCRIPT_DIR}/wait_for_ledger.sh" localhost 6865

# --- Build and upload the production DAR (CoreState lives there). ---
log "building wormhole-core DAR"
(cd "${REPO_ROOT}/canton" && dpm build --all >/dev/null)
CORE_VERSION="$(awk '/^version:/ {print $2}' "${REPO_ROOT}/canton/core/daml.yaml")"
CORE_DAR="${REPO_ROOT}/canton/core/.daml/dist/wormhole-core-${CORE_VERSION}.dar"

UPLOAD_SCRIPT="${WORK_DIR}/upload_dar.canton"
cat > "${UPLOAD_SCRIPT}" <<EOF
val p = sandbox
println("PACKAGE_ID=" + p.dars.upload("${CORE_DAR}"))
EOF
# The Ledger API port accepts connections slightly before the participant has
# finished auto-connecting to its built-in synchronizer; DAR upload needs a
# synchronizer to vet against, so retry briefly rather than racing it.
GG_CORE_PACKAGE_ID=""
for _ in $(seq 1 15); do
  UPLOAD_OUT="$(dpm canton-console --no-tty --bootstrap "${UPLOAD_SCRIPT}" < /dev/null 2>&1)"
  GG_CORE_PACKAGE_ID="$(echo "${UPLOAD_OUT}" | grep -oE 'PACKAGE_ID=[a-f0-9]+' | cut -d= -f2)"
  [ -n "${GG_CORE_PACKAGE_ID}" ] && break
  sleep 2
done
if [ -z "${GG_CORE_PACKAGE_ID}" ]; then
  fail "could not upload/vet wormhole-core DAR"
  echo "${UPLOAD_OUT}"
  exit 1
fi
log "uploaded wormhole-core DAR, package id ${GG_CORE_PACKAGE_ID}"
export GG_CORE_PACKAGE_ID

# --- Set up the 2-of-3 guardianGovernance party. ---
export GG_GENERATE_TEST_KEYS=1
export GG_KEY_DIR="${WORK_DIR}/guardian-keys"
export GG_ARTIFACTS_FILE="${WORK_DIR}/guardian-artifacts.json"
export GG_THRESHOLD=2
export GG_PARTY_NAME=guardianGovernance
log "running setup_guardian_governance.sh (guardianGovernance, 2-of-3)"
if ! "${SCRIPT_DIR}/setup_guardian_governance.sh" > "${WORK_DIR}/setup_gg.log" 2>&1; then
  fail "setup_guardian_governance.sh (guardianGovernance) exited non-zero"
  cat "${WORK_DIR}/setup_gg.log"
  exit 1
fi

# --- Set up a single-key (1-of-1) external Operator party, reusing the same
# topology script (see genesis_guardian_governance.sh's header for why a
# second external party is needed: interactive submission requires every
# actAs party, not just guardianGovernance, to sign externally). ---
export GG_KEY_DIR="${WORK_DIR}/operator-keys"
export GG_ARTIFACTS_FILE="${WORK_DIR}/operator-artifacts.json"
export GG_THRESHOLD=1
export GG_PARTY_NAME=Operator
mkdir -p "${GG_KEY_DIR}"
node "${SCRIPT_DIR}/guardian_key_tool.js" generate "${GG_KEY_DIR}/owner1" >/dev/null
export GG_GENERATE_TEST_KEYS=0
export GG_OWNER_PUBKEY_1="${GG_KEY_DIR}/owner1.pub"
export GG_OWNER_PUBKEY_2="${GG_KEY_DIR}/owner1.pub"
export GG_OWNER_PUBKEY_3="${GG_KEY_DIR}/owner1.pub"
export GG_OWNER_SIGN_CMD_1="node ${SCRIPT_DIR}/guardian_key_tool.js sign ${GG_KEY_DIR}/owner1.key"
export GG_OWNER_SIGN_CMD_2="${GG_OWNER_SIGN_CMD_1}"
export GG_OWNER_SIGN_CMD_3="${GG_OWNER_SIGN_CMD_1}"
log "running setup_guardian_governance.sh (Operator, 1-of-1)"
if ! "${SCRIPT_DIR}/setup_guardian_governance.sh" > "${WORK_DIR}/setup_op.log" 2>&1; then
  fail "setup_guardian_governance.sh (Operator) exited non-zero"
  cat "${WORK_DIR}/setup_op.log"
  exit 1
fi

GG_ARTIFACTS_FILE="${WORK_DIR}/guardian-artifacts.json"
OP_ARTIFACTS_FILE="${WORK_DIR}/operator-artifacts.json"

# --- Assertions: topology shape. ---
OWNER_COUNT="$(jq '.owners | length' "${GG_ARTIFACTS_FILE}")"
THRESHOLD_VALUE="$(jq -r '.threshold' "${GG_ARTIFACTS_FILE}")"
HOSTING_PERMISSION="$(jq -r '.hostingPermission' "${GG_ARTIFACTS_FILE}")"
PARTY_ID="$(jq -r '.partyId' "${GG_ARTIFACTS_FILE}")"
DN_ID="$(jq -r '.decentralizedNamespace' "${GG_ARTIFACTS_FILE}")"

if [ "${OWNER_COUNT}" = "3" ]; then pass "decentralized namespace has 3 owners"; else fail "expected 3 owners, got ${OWNER_COUNT}"; fi
if [ "${THRESHOLD_VALUE}" = "2" ]; then pass "decentralized namespace threshold is 2"; else fail "expected threshold 2, got ${THRESHOLD_VALUE}"; fi
if [ "${HOSTING_PERMISSION}" = "Confirmation" ]; then pass "party hosted at Confirmation"; else fail "expected Confirmation, got ${HOSTING_PERMISSION}"; fi
if [ "${PARTY_ID}" = "guardianGovernance::${DN_ID}" ]; then
  pass "party id is guardianGovernance::<dn namespace>"
else
  fail "party id '${PARTY_ID}' does not equal guardianGovernance::${DN_ID}"
fi

# --- Assertions: genesis interactive submission. ---
export GG_ARTIFACTS_FILE
export GG_OPERATOR_ARTIFACTS_FILE="${OP_ARTIFACTS_FILE}"

# Columns: mode | GENESIS_LAST_CREATE | expected exit status | CoreState result | EmitterRegistry result
# ("-": no RESULT line) | rejection reason ("-" for none) | description. The rows run in order on one ledger.
GENESIS_CASES=(
  "threshold|EmitterRegistry|1|rejected|-|Received 1 valid signatures from distinct keys (0 invalid), but expected at least 2 valid for guardianGovernance::|1-of-3 co-signed genesis (threshold)"
  "forge|EmitterRegistry|1|rejected|-|The following actAs parties did not provide an external signature: guardianGovernance::|operator-only genesis (anti-forgery)"
  "happy|CoreState|0|committed|-|-|2-of-3 co-signed CoreState alone"
  "happy|EmitterRegistry|0|exists|committed|-|2-of-3 rerun creates only the missing EmitterRegistry"
  "happy|EmitterRegistry|0|exists|exists|-|2-of-3 rerun creates nothing"
)
for genesis_case_index in "${!GENESIS_CASES[@]}"; do
  IFS='|' read -r genesis_mode last_create expected_status core_state_result registry_result rejection_reason description \
    <<<"${GENESIS_CASES[${genesis_case_index}]}"
  genesis_log="${WORK_DIR}/genesis_${genesis_case_index}_${genesis_mode}.log"
  log "genesis: ${description}, expect exit ${expected_status}"
  GENESIS_LAST_CREATE="${last_create}" "${SCRIPT_DIR}/genesis_guardian_governance.sh" "${genesis_mode}" > "${genesis_log}" 2>&1
  genesis_status=$?
  if [ "${genesis_status}" -eq "${expected_status}" ]; then
    pass "${description}: exit ${genesis_status}"
  else
    fail "${description}: exit ${genesis_status}, expected ${expected_status}"
    cat "${genesis_log}"
  fi
  for genesis_template_result in "CoreState|${core_state_result}" "EmitterRegistry|${registry_result}"; do
    IFS='|' read -r genesis_template expected_result <<<"${genesis_template_result}"
    actual_results="$(grep -E "^RESULT ${genesis_template} " "${genesis_log}" | xargs)"
    expected_results="-"
    [ "${expected_result}" = "-" ] || expected_results="RESULT ${genesis_template} ${expected_result}"
    [ -n "${actual_results}" ] || actual_results="-"
    if [ "${actual_results}" = "${expected_results}" ]; then
      pass "${description}: ${genesis_template} ${expected_result}"
    else
      fail "${description}: ${genesis_template} gave '${actual_results}', expected '${expected_results}'"
      grep -E 'RESULT|failed|HTTP|genesis:' "${genesis_log}" | head -n 10
    fi
  done
  if [ "${rejection_reason}" != "-" ]; then
    rejection_reason_count="$(grep -cF -- "${rejection_reason}" "${genesis_log}")"
    if [ "${rejection_reason_count}" = "1" ]; then
      pass "${description}: the rejection gives: ${rejection_reason}"
    else
      fail "${description}: ${rejection_reason_count} of 1 rejection gives: ${rejection_reason}"
    fi
  fi
done
echo
if [ "${FAILURES}" -eq 0 ]; then
  log "ALL CHECKS PASSED"
  exit 0
else
  log "${FAILURES} CHECK(S) FAILED"
  exit 1
fi
