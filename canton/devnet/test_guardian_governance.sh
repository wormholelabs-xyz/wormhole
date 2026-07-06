#!/usr/bin/env bash
#
# Local end-to-end test for the guardianGovernance external-party bootstrap.
# Starts (or reuses) a `dpm sandbox`, runs setup_guardian_governance.sh, then
# asserts the §4 test table from the design plan:
#
#   - DN created: 3 owners, threshold 2.
#   - Party allocated: guardianGovernance::<dn>, hosted at Confirmation,
#     2-of-3 party signing keys.
#   - 2-of-3 co-signs `CoreState` genesis via interactive submission: succeeds.
#   - 1-of-3 co-signs: rejected (threshold enforced).
#   - 0-of-3 (operator alone): rejected (anti-forgery -- the crux property).
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

cleanup() {
  if [ -n "${SANDBOX_PID}" ]; then
    log "stopping sandbox (pid ${SANDBOX_PID})"
    kill "${SANDBOX_PID}" 2>/dev/null || true
    wait "${SANDBOX_PID}" 2>/dev/null || true
    # `dpm sandbox` forks the actual JVM as a child rather than exec-ing into
    # it, so killing $! (the `dpm` process) can leave the java process
    # running, reparented to init. Belt and suspenders: also reap any
    # sandbox JVM still listening on our target ports.
    pkill -f "canton-open-source.*sandbox" 2>/dev/null || true
  fi
}
trap cleanup EXIT

if [ "${GG_TEST_REUSE_SANDBOX:-0}" != "1" ]; then
  log "starting dpm sandbox in ${WORK_DIR}"
  (cd "${WORK_DIR}" && exec dpm sandbox --no-tty -C canton.participants.sandbox.ledger-api.address=0.0.0.0) \
    > "${WORK_DIR}/sandbox.log" 2>&1 &
  SANDBOX_PID=$!
fi

"${SCRIPT_DIR}/wait_for_ledger.sh" localhost 6865

# --- Build and upload the production DAR (CoreState lives there). ---
log "building wormhole-core DAR"
(cd "${REPO_ROOT}/canton" && dpm build --all >/dev/null)
CORE_DAR="${REPO_ROOT}/canton/core/.daml/dist/wormhole-core-0.1.0.dar"

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

[ "${OWNER_COUNT}" = "3" ] && pass "decentralized namespace has 3 owners" || fail "expected 3 owners, got ${OWNER_COUNT}"
[ "${THRESHOLD_VALUE}" = "2" ] && pass "decentralized namespace threshold is 2" || fail "expected threshold 2, got ${THRESHOLD_VALUE}"
[ "${HOSTING_PERMISSION}" = "Confirmation" ] && pass "party hosted at Confirmation" || fail "expected Confirmation, got ${HOSTING_PERMISSION}"
if [ "${PARTY_ID}" = "guardianGovernance::${DN_ID}" ]; then
  pass "party id is guardianGovernance::<dn namespace>"
else
  fail "party id '${PARTY_ID}' does not equal guardianGovernance::${DN_ID}"
fi

# --- Assertions: genesis interactive submission. ---
export GG_ARTIFACTS_FILE
export GG_OPERATOR_ARTIFACTS_FILE="${OP_ARTIFACTS_FILE}"

log "genesis: 2-of-3 (happy path, expect success)"
if "${SCRIPT_DIR}/genesis_guardian_governance.sh" happy > "${WORK_DIR}/genesis_happy.log" 2>&1; then
  pass "2-of-3 co-signed CoreState genesis succeeded"
else
  fail "2-of-3 co-signed CoreState genesis was rejected"
  cat "${WORK_DIR}/genesis_happy.log"
fi

log "genesis: 1-of-3 (expect rejection)"
if "${SCRIPT_DIR}/genesis_guardian_governance.sh" threshold > "${WORK_DIR}/genesis_threshold.log" 2>&1; then
  fail "1-of-3 co-signed CoreState genesis was accepted (threshold not enforced!)"
else
  pass "1-of-3 co-signed CoreState genesis was rejected"
fi

log "genesis: 0-of-3 / operator-only (anti-forgery, expect rejection)"
if "${SCRIPT_DIR}/genesis_guardian_governance.sh" forge > "${WORK_DIR}/genesis_forge.log" 2>&1; then
  fail "operator-only CoreState genesis was accepted (forgery not blocked!)"
else
  pass "operator-only CoreState genesis was rejected (forgery blocked)"
fi

echo
if [ "${FAILURES}" -eq 0 ]; then
  log "ALL CHECKS PASSED"
  exit 0
else
  log "${FAILURES} CHECK(S) FAILED"
  exit 1
fi
