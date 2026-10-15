#!/usr/bin/env bash
# Localnet run of the off-chain tools against a standalone surfpool:
# load the build-devnet programs at the build's program ids, fund the test
# backfill authorities, write svm-accountant.toml, then run backfill and the
# parity check for WTT and NTT, and one tampered-snapshot check that must fail.
#
# Usage (from svm/accountant, through just):
#   just tools-localnet          run, then stop surfpool
#   LOCALNET_KEEP=1 just tools-localnet   leave surfpool running for inspection
#   just tools-localnet-stop     stop a surfpool left running
#
# Needs the seven build addresses in the environment (just's test_env), the
# build-devnet .so files, a debug build of the tools, surfpool, curl, openssl
# and python3 (standard library only).

set -euo pipefail

TOOLS="$(cd "$(dirname "$0")" && pwd)"
ACCOUNTANT="$(dirname "$TOOLS")"
WORK="$TOOLS/.localnet"
BIN="$TOOLS/target/debug"
PORT="${LOCALNET_PORT:-8899}"
RPC="http://127.0.0.1:$PORT"
# Upper bound on every wait loop, in seconds.
MAX_WAIT_SECONDS=90

log() { printf '[localnet] %s\n' "$*"; }
fail() { printf '[localnet] FAIL: %s\n' "$*" >&2; exit 1; }

# The surfpool process of this script's port, by its command line.
surfpool_pattern() { printf 'surfpool start .*--port %s ' "$1"; }

stop_surfpool() {
    local port_file="$WORK/surfpool.port"
    [ -f "$port_file" ] || return 0
    local port
    port="$(cat "$port_file")"
    if pkill -f "$(surfpool_pattern "$port")"; then
        log "stopped surfpool on port $port"
    fi
    rm -f "$port_file"
}

if [ "${1:-}" = "stop" ]; then
    stop_surfpool
    exit 0
fi

for name in GLOBAL_ACCOUNTANT_PROGRAM_ID NTT_GLOBAL_ACCOUNTANT_PROGRAM_ID NOREPLAY_PROGRAM_ID \
    BACKFILL_AUTHORITY NTT_BACKFILL_AUTHORITY; do
    [ -n "${!name:-}" ] || fail "$name is not set; run through \`just tools-localnet\`"
done
for tool in surfpool curl openssl python3; do
    command -v "$tool" >/dev/null || fail "$tool is not on PATH"
done
for so in "$ACCOUNTANT/target/deploy/global_accountant_backfill.so" \
    "$ACCOUNTANT/target/deploy/ntt_global_accountant_backfill.so" \
    "$ACCOUNTANT/crates/test-fixtures/data/solana_noreplay.so" \
    "$BIN/svm-accountant-backfill" "$BIN/svm-accountant-parity"; do
    [ -f "$so" ] || fail "missing $so"
done

stop_surfpool
rm -rf "$WORK"
mkdir -p "$WORK"/{keys,snapshots,ledgers,progress,surfpool}

rpc() {
    curl -sf -X POST -H 'Content-Type: application/json' --data "@$1" "$RPC"
}

# ---------------------------------------------------------------- surfpool
if curl -sf -o /dev/null -X POST -H 'Content-Type: application/json' \
    --data '{"jsonrpc":"2.0","id":1,"method":"getHealth"}' "$RPC"; then
    fail "port $PORT already serves an RPC; stop it or set LOCALNET_PORT"
fi
echo "$PORT" >"$WORK/surfpool.port"
(
    cd "$WORK/surfpool"
    nohup surfpool start --no-tui --no-studio --no-deploy -y --offline \
        --port "$PORT" --ws-port "$((PORT + 1))" --studio-port "$((PORT + 9589))" \
        --slot-time 400 --log-level warn </dev/null >"$WORK/surfpool.log" 2>&1 &
)
if [ "${LOCALNET_KEEP:-0}" != "1" ]; then
    trap stop_surfpool EXIT
fi
log "surfpool starting on $RPC (log: $WORK/surfpool.log)"
printf '{"jsonrpc":"2.0","id":1,"method":"getHealth"}' >"$WORK/health.json"
for _ in $(seq "$MAX_WAIT_SECONDS"); do
    rpc "$WORK/health.json" 2>/dev/null | grep -q '"ok"' && break
    sleep 1
done
rpc "$WORK/health.json" | grep -q '"ok"' || fail "surfpool not healthy after ${MAX_WAIT_SECONDS}s"

# ---------------------------------------------------------------- programs
write_program() {
    python3 - "$1" "$2" "$WORK/request.json" <<'EOF'
import json, sys
program_id, so_path, out = sys.argv[1:4]
with open(so_path, "rb") as f:
    body = {"jsonrpc": "2.0", "id": 1, "method": "surfnet_writeProgram",
            "params": [program_id, f.read().hex(), 0]}
json.dump(body, open(out, "w"))
EOF
    rpc "$WORK/request.json" | grep -q '"result"' || fail "surfnet_writeProgram $1"
    log "loaded $(basename "$2") at $1"
}
write_program "$GLOBAL_ACCOUNTANT_PROGRAM_ID" "$ACCOUNTANT/target/deploy/global_accountant_backfill.so"
write_program "$NTT_GLOBAL_ACCOUNTANT_PROGRAM_ID" "$ACCOUNTANT/target/deploy/ntt_global_accountant_backfill.so"
write_program "$NOREPLAY_PROGRAM_ID" "$ACCOUNTANT/crates/test-fixtures/data/solana_noreplay.so"

# ---------------------------------------------------------------- authorities
# Test authorities: ed25519 seeds of 32 × 0x01 (WTT) and 32 × 0x02 (NTT).
write_keypair() {
    local name="$1" byte="$2" want="$3" seed der pub
    seed="$(python3 -c 'import sys; print(sys.argv[1] * 32)' "$byte")"
    der="$WORK/keys/$name.der"
    printf '302e020100300506032b657004220420%s' "$seed" | xxd -r -p >"$der"
    pub="$(openssl pkey -inform DER -in "$der" -pubout -outform DER | tail -c 32 | xxd -p | tr -d '\n')"
    rm -f "$der"
    python3 - "$seed" "$pub" "$want" "$WORK/keys/$name.json" <<'EOF'
import json, sys
seed, pub, want, out = sys.argv[1:5]
raw = bytes.fromhex(pub)
alphabet = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz"
n, encoded = int.from_bytes(raw, "big"), ""
while n:
    n, rem = divmod(n, 58)
    encoded = alphabet[rem] + encoded
encoded = "1" * (len(raw) - len(raw.lstrip(b"\0"))) + encoded
if encoded != want:
    sys.exit(f"derived {encoded}, build expects {want}")
json.dump(list(bytes.fromhex(seed) + raw), open(out, "w"))
EOF
    printf '{"jsonrpc":"2.0","id":1,"method":"requestAirdrop","params":["%s",1000000000000]}' \
        "$want" >"$WORK/request.json"
    rpc "$WORK/request.json" | grep -q '"result"' || fail "airdrop to $want"
    log "funded $name $want"
}
write_keypair wtt-authority 01 "$BACKFILL_AUTHORITY"
write_keypair ntt-authority 02 "$NTT_BACKFILL_AUTHORITY"

# ---------------------------------------------------------------- config
cp "$TOOLS/common/fixtures/wtt_sample_snapshot.jsonl" "$WORK/snapshots/wtt.jsonl"
cp "$TOOLS/common/fixtures/ntt_mainnet_subset_snapshot.jsonl" "$WORK/snapshots/ntt.jsonl"
cat >"$WORK/svm-accountant.toml" <<EOF
rpc_url = "$RPC"

[backfill]
concurrency = 8

[wtt]
snapshot = "snapshots/wtt.jsonl"
ledger = "ledgers/wtt-parity.jsonl"
progress_file = "progress/wtt.json"
payer = "keys/wtt-authority.json"

[ntt]
snapshot = "snapshots/ntt.jsonl"
ledger = "ledgers/ntt-parity.jsonl"
progress_file = "progress/ntt.json"
payer = "keys/ntt-authority.json"
EOF

# WTT snapshot with one balance changed and one extra transfer: exactly 2 findings.
python3 - "$WORK/snapshots/wtt.jsonl" "$WORK/snapshots/wtt-tampered.jsonl" <<'EOF'
import json, sys
rows = [json.loads(line) for line in open(sys.argv[1]) if line.strip()]
balance = next(r for r in rows if r["kind"] == "account")
balance["balance"] = "0x" + "00" * 31 + "01"
transfer = next(r for r in rows if r["kind"] == "transfer")
extra = dict(transfer, sequence=transfer["sequence"] + 1000)
rows.insert(rows.index(transfer) + 1, extra)
open(sys.argv[2], "w").write("".join(json.dumps(r) + "\n" for r in rows))
EOF
sed -e 's#snapshots/wtt.jsonl#snapshots/wtt-tampered.jsonl#' \
    -e 's#ledgers/wtt-parity.jsonl#ledgers/wtt-tampered.jsonl#' \
    "$WORK/svm-accountant.toml" >"$WORK/tampered.toml"

# ---------------------------------------------------------------- steps
STEP_LOG="$WORK/steps"
mkdir -p "$STEP_LOG"
step=0
# expect <exit code> <name> <command...>; output goes to .localnet/steps/<n>-<name>.log
expect() {
    local want="$1" name="$2"
    shift 2
    step=$((step + 1))
    local out="$STEP_LOG/$step-$name.log" got=0
    (cd "$WORK" && "$@") >"$out" 2>&1 || got=$?
    if [ "$got" != "$want" ]; then
        tail -20 "$out" >&2
        fail "$name exited $got, want $want (log: $out)"
    fi
    log "PASS $name (exit $got)"
}

expect 0 run-wtt "$BIN/svm-accountant-backfill" run wtt
expect 0 check-wtt "$BIN/svm-accountant-parity" check wtt --strict
expect 0 report-wtt "$BIN/svm-accountant-parity" report wtt
expect 0 resume-wtt "$BIN/svm-accountant-backfill" run wtt
grep -q "0 to submit this run" "$STEP_LOG/$step-resume-wtt.log" ||
    fail "resume-wtt resubmitted batches"
expect 0 run-ntt "$BIN/svm-accountant-backfill" run ntt
expect 0 check-ntt "$BIN/svm-accountant-parity" check ntt --strict
expect 0 report-ntt "$BIN/svm-accountant-parity" report ntt
expect 1 check-wtt-tampered "$BIN/svm-accountant-parity" --config tampered.toml check wtt --strict
grep -q "PARITY: 2 finding(s)" "$STEP_LOG/$step-check-wtt-tampered.log" ||
    fail "check-wtt-tampered did not report exactly the 2 planted findings"

log "all $step steps passed; files in $WORK"
if [ "${LOCALNET_KEEP:-0}" = "1" ]; then
    log "surfpool left running on $RPC; stop it with \`just tools-localnet-stop\`"
fi
