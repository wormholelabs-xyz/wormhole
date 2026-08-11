#!/usr/bin/env bash
#
# Bootstrap `guardianGovernance` as an external, decentralized-namespace party
# (default 2-of-3 guardian keys) by running the guardian_governance.canton
# console script against a running participant. Mirrors bootstrap.sh's shape:
# wait for the Ledger API, then run the scripted step.
#
# Two modes, selected by GG_GENERATE_TEST_KEYS:
#   1 (default)  Local/test: generate three Ed25519 keypairs on this host with
#                guardian_key_tool.js and use them as the three owners. This
#                is what test_guardian_governance.sh uses; it is NOT a
#                production key-custody model (see below).
#   0            Production-shaped: the caller supplies GG_OWNER_PUBKEY_1/2/3
#                (DER-encoded Ed25519 public keys already exported from
#                guardian custody -- HSM/KMS/offline signer) and
#                GG_OWNER_SIGN_CMD_1/2/3 (a command per owner that signs a
#                hex hash and prints a hex signature, backed by that same
#                custody system). No private key material is generated or
#                touched by this script in this mode.
set -euo pipefail

HOST="${CANTON_HOST:-localhost}"
PORT="${CANTON_LEDGER_API_PORT:-6865}"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

GENERATE_TEST_KEYS="${GG_GENERATE_TEST_KEYS:-1}"
KEY_DIR="${GG_KEY_DIR:-/tmp/guardian-governance-keys}"
export GG_ARTIFACTS_FILE="${GG_ARTIFACTS_FILE:-/tmp/guardian-governance-artifacts.json}"
export GG_THRESHOLD="${GG_THRESHOLD:-2}"
export GG_PARTY_NAME="${GG_PARTY_NAME:-guardianGovernance}"

"${SCRIPT_DIR}/wait_for_ledger.sh" "${HOST}" "${PORT}"

if [ "${GENERATE_TEST_KEYS}" = "1" ]; then
  echo "[guardian-governance] GG_GENERATE_TEST_KEYS=1: generating three local test owner keys in ${KEY_DIR}"
  echo "[guardian-governance] (production must set GG_GENERATE_TEST_KEYS=0 and supply real custody-backed keys/signers -- see this script's header)"
  mkdir -p "${KEY_DIR}"
  for i in 1 2 3; do
    node "${SCRIPT_DIR}/guardian_key_tool.js" generate "${KEY_DIR}/owner${i}" >/dev/null
  done
  export GG_OWNER_PUBKEY_1="${KEY_DIR}/owner1.pub"
  export GG_OWNER_PUBKEY_2="${KEY_DIR}/owner2.pub"
  export GG_OWNER_PUBKEY_3="${KEY_DIR}/owner3.pub"
  export GG_OWNER_SIGN_CMD_1="node ${SCRIPT_DIR}/guardian_key_tool.js sign ${KEY_DIR}/owner1.key"
  export GG_OWNER_SIGN_CMD_2="node ${SCRIPT_DIR}/guardian_key_tool.js sign ${KEY_DIR}/owner2.key"
  export GG_OWNER_SIGN_CMD_3="node ${SCRIPT_DIR}/guardian_key_tool.js sign ${KEY_DIR}/owner3.key"
else
  : "${GG_OWNER_PUBKEY_1:?GG_OWNER_PUBKEY_1 is required when GG_GENERATE_TEST_KEYS=0}"
  : "${GG_OWNER_PUBKEY_2:?GG_OWNER_PUBKEY_2 is required when GG_GENERATE_TEST_KEYS=0}"
  : "${GG_OWNER_PUBKEY_3:?GG_OWNER_PUBKEY_3 is required when GG_GENERATE_TEST_KEYS=0}"
  : "${GG_OWNER_SIGN_CMD_1:?GG_OWNER_SIGN_CMD_1 is required when GG_GENERATE_TEST_KEYS=0}"
  : "${GG_OWNER_SIGN_CMD_2:?GG_OWNER_SIGN_CMD_2 is required when GG_GENERATE_TEST_KEYS=0}"
  : "${GG_OWNER_SIGN_CMD_3:?GG_OWNER_SIGN_CMD_3 is required when GG_GENERATE_TEST_KEYS=0}"
fi

CONSOLE_CONFIG_ARGS=()
if [ -n "${GG_CANTON_CONSOLE_CONFIG:-}" ]; then
  # Production: point the console at a real participant/synchronizer config
  # instead of the sandbox default (`dpm canton-console` connects to sandbox
  # on localhost unless given an explicit config).
  CONSOLE_CONFIG_ARGS=(-c "${GG_CANTON_CONSOLE_CONFIG}")
fi

echo "[guardian-governance] running guardian_governance.canton"
dpm canton-console --no-tty "${CONSOLE_CONFIG_ARGS[@]}" --bootstrap "${SCRIPT_DIR}/guardian_governance.canton" < /dev/null

echo "[guardian-governance] artifacts:"
cat "${GG_ARTIFACTS_FILE}"
