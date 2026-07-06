#!/usr/bin/env bash
#
# Bootstrap the Wormhole core bridge on the Canton sandbox: allocate the Operator
# party and create the CoreState (with the devnet guardian) by running the
# Test.TestCore:setup Daml Script.
#
# The guardian does NOT need the Operator party id — the watcher uses a wildcard
# "any party" filter (see canton/README.md §7.1). The party is surfaced below for
# information only.
set -euo pipefail

# Reach the sandbox over localhost: this runs in the same pod as the canton-node
# container, and the `canton` Service has no endpoint until the pod is Ready —
# which won't happen until this bootstrap finishes (the /canton/success readiness
# gate), so using the Service name here would deadlock.
HOST="${CANTON_HOST:-localhost}"
PORT="${CANTON_LEDGER_API_PORT:-6865}"
# The test DAR carries the Test.TestCore:setup script and packs core's DALFs, so
# --upload-dar uploads (and vets) the core package too.
DAR=/canton/wormhole-core-test.dar
RESULT=/canton/setup-result.json

/canton/devnet/wait_for_ledger.sh "${HOST}" "${PORT}"

echo "[canton] running Test.TestCore:setup"
# Canton Network toolchain: `dpm script`. --upload-dar uploads AND vets the DAR
# (synchronously) before running, which is required on Canton 3.x — a bare
# upload races package vetting and the first create fails with
# PACKAGE_SELECTION_FAILED. Note the flag is --ledger-port (not --port).
dpm script \
  --dar "${DAR}" \
  --upload-dar yes \
  --script-name Test.TestCore:setup \
  --ledger-host "${HOST}" \
  --ledger-port "${PORT}" \
  --output-file "${RESULT}"

# The setup script returns a tuple {"_1": <Operator>, "_2": <GuardianObserver>,
# "_3": <ContractId>} as JSON. Party ids are the quoted values containing "::"
# (the namespace separator); the contract id has none. The parties appear in
# result order, so _1 is the operator and _2 is the read-only guardianObserver
# (the party a production guardian reads as via --cantonReadAsParty). The devnet
# sandbox is a single participant, so no topology work is needed; both ids are
# surfaced for information / wiring only.
OPERATOR_PARTY="$(grep -oE '"[^"]*::[^"]*"' "${RESULT}" | sed -n '1p' | tr -d '"')"
OBSERVER_PARTY="$(grep -oE '"[^"]*::[^"]*"' "${RESULT}" | sed -n '2p' | tr -d '"')"
echo "[canton] setup complete; Operator party: ${OPERATOR_PARTY}"
echo "[canton] guardianObserver party (read-only; --cantonReadAsParty): ${OBSERVER_PARTY}"

touch /canton/success
echo "[canton] bootstrap done; sleeping"
exec sleep infinity
