//! Surfpool E2E spike. Drives a real `surfpool start` subprocess over
//! JSON-RPC through the same DigestAccount open/close lifecycle the mollusk
//! tests already cover. Not part of the default `cargo test` set; gated behind
//! `#[ignore]`.
//!
//! # Run
//!
//! ```sh
//! # From svm/global-accountant/
//! make test-e2e-spike
//! # Or manually:
//! cargo build-sbf --features bpf-entrypoint,mock-vaa,test-only-open-digest
//! SBF_OUT_DIR=$(pwd)/target/deploy \
//!   cargo test --features mock-vaa,test-only-open-digest \
//!   --test surfpool_e2e_spike -- --ignored --nocapture
//! ```
//!
//! # Prerequisites
//!
//! - `surfpool` (>= v1.2.1) on `$PATH` or in `~/.local/bin`. Install via
//!   `curl -sL https://run.surfpool.run/ | bash`.
//! - The dev `.so` built with `mock-vaa,test-only-open-digest` at
//!   `target/deploy/global_accountant.so` (`make build-dev`).
//!
//! # What the spike proves
//!
//! 1. surfpool boots cleanly in `--offline` mode (no datasource).
//! 2. `surfnet_writeProgram` deploys a Pinocchio `.so` and the BPF Upgradeable
//!    Loader marks the account executable.
//! 3. Our entrypoint dispatches `OpenDigest` and `CloseDigest` via a real
//!    RPC-driven transaction (vs. the in-process mollusk path).
//! 4. PDA layout matches the on-chain `DigestAccountLayout`.
//! 5. rent flows back to the payer on close, and the PDA returns to the System
//!    Program.
//!
//! The subprocess is owned by a guard struct; its `Drop` impl SIGKILLs the
//! child so panics or assertion failures never leak a surfpool instance.

use std::{
    io::{BufRead, BufReader, Read},
    net::TcpListener,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use global_accountant_definitions::{
    DigestAccountLayout, Instruction as IxDiscriminator, DIGEST_SEED_PREFIX,
    VERIFY_VAA_SHIM_PROGRAM_ID,
};
use solana_client::rpc_client::RpcClient;
use solana_commitment_config::CommitmentConfig;
use solana_instruction::{AccountMeta, Instruction};
use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use solana_signer::Signer;
use solana_system_interface::program as system_program;
use solana_transaction::Transaction;

const SURFPOOL_BOOT_TIMEOUT: Duration = Duration::from_secs(30);
const RPC_READY_POLL_INTERVAL: Duration = Duration::from_millis(250);

/// Owns the surfpool child process plus its piped stdout/stderr drain threads.
/// `Drop` ensures the child is killed even if a test panics.
struct SurfpoolGuard {
    child: Child,
    rpc_port: u16,
    _stdout_pump: Option<thread::JoinHandle<()>>,
    _stderr_pump: Option<thread::JoinHandle<()>>,
}

impl SurfpoolGuard {
    fn rpc_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.rpc_port)
    }
}

impl Drop for SurfpoolGuard {
    fn drop(&mut self) {
        // Best-effort SIGKILL; surfpool does not need a graceful shutdown for
        // this spike. `wait` reaps the zombie.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Pick a free TCP port by binding to 0 and reading the assigned port. Brief
/// race window between the listener drop and surfpool bind — acceptable for
/// a spike test.
fn free_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral");
    listener.local_addr().expect("local_addr").port()
}

/// Resolve the `surfpool` binary. Falls back to `~/.local/bin/surfpool` since
/// the official installer puts it there and that directory is not always on
/// `$PATH` for `cargo test` invocations.
fn surfpool_binary() -> PathBuf {
    if let Ok(found) = which::which_global("surfpool") {
        return found;
    }
    let home = std::env::var("HOME").unwrap_or_default();
    let candidate = PathBuf::from(home).join(".local/bin/surfpool");
    if candidate.exists() {
        return candidate;
    }
    panic!(
        "`surfpool` not on PATH and not at ~/.local/bin/surfpool. \
         Install with: curl -sL https://run.surfpool.run/ | bash"
    );
}

/// Tiny `which` shim — avoids pulling a dev-dep just to resolve a binary.
mod which {
    use std::path::PathBuf;

    pub fn which_global(name: &str) -> Result<PathBuf, ()> {
        let path = std::env::var_os("PATH").ok_or(())?;
        for entry in std::env::split_paths(&path) {
            let candidate = entry.join(name);
            if candidate.is_file() {
                return Ok(candidate);
            }
        }
        Err(())
    }
}

fn start_surfpool() -> SurfpoolGuard {
    let bin = surfpool_binary();
    let rpc_port = free_port();
    let ws_port = free_port();
    let studio_port = free_port();

    // CWD is a fresh per-test scratch dir so surfpool's `.surfpool/` artifacts
    // (logs, runbook scaffolding) don't bleed across runs.
    let scratch = std::env::temp_dir().join(format!("ga-surfpool-spike-{}", rpc_port));
    let _ = std::fs::remove_dir_all(&scratch);
    std::fs::create_dir_all(&scratch).expect("create scratch dir");

    eprintln!(
        "[spike] starting surfpool: bin={} rpc={} ws={} studio={} cwd={}",
        bin.display(),
        rpc_port,
        ws_port,
        studio_port,
        scratch.display()
    );

    let mut cmd = Command::new(&bin);
    cmd.arg("start")
        .arg("--no-tui")
        .arg("--no-studio")
        .arg("--offline") // Pure local simnet — no mainnet datasource for the spike.
        .arg("--no-deploy") // We deploy programmatically via surfnet_writeProgram.
        .arg("-y") // Skip prompts.
        .arg("--port")
        .arg(rpc_port.to_string())
        .arg("--ws-port")
        .arg(ws_port.to_string())
        .arg("--studio-port")
        .arg(studio_port.to_string())
        .arg("--slot-time")
        .arg("100") // 100ms slots for snappier confirmations.
        .arg("--log-level")
        .arg("warn")
        .current_dir(&scratch)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = cmd.spawn().expect("spawn surfpool");

    // Drain stdout/stderr into the test's stderr so `--nocapture` shows any
    // failure context. Without these pumps the pipes fill up and surfpool can
    // block on writes.
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let stdout_pump = stdout.map(|s| {
        thread::spawn(move || {
            let reader = BufReader::new(s);
            for line in reader.lines().map_while(Result::ok) {
                eprintln!("[surfpool stdout] {line}");
            }
        })
    });
    let stderr_pump = stderr.map(|s| {
        thread::spawn(move || {
            let reader = BufReader::new(s);
            for line in reader.lines().map_while(Result::ok) {
                eprintln!("[surfpool stderr] {line}");
            }
        })
    });

    let guard = SurfpoolGuard {
        child,
        rpc_port,
        _stdout_pump: stdout_pump,
        _stderr_pump: stderr_pump,
    };

    wait_for_rpc_ready(&guard);
    guard
}

fn wait_for_rpc_ready(guard: &SurfpoolGuard) {
    let client =
        RpcClient::new_with_commitment(guard.rpc_url(), CommitmentConfig::confirmed());
    let deadline = Instant::now() + SURFPOOL_BOOT_TIMEOUT;
    let mut last_err: Option<String> = None;
    while Instant::now() < deadline {
        match client.get_health() {
            Ok(_) => {
                eprintln!("[spike] surfpool RPC ready at {}", guard.rpc_url());
                return;
            }
            Err(e) => last_err = Some(e.to_string()),
        }
        thread::sleep(RPC_READY_POLL_INTERVAL);
    }
    panic!(
        "surfpool RPC at {} did not become healthy within {:?}: {:?}",
        guard.rpc_url(),
        SURFPOOL_BOOT_TIMEOUT,
        last_err
    );
}

/// Path to the dev-built `.so`. The Makefile target `test-e2e-spike` runs
/// `make build-dev` first; this helper just locates the artifact.
fn so_path() -> PathBuf {
    // `target/deploy/global_accountant.so` relative to the workspace root.
    // `CARGO_MANIFEST_DIR` for this crate is .../programs/global-accountant.
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = manifest_dir
        .parent()
        .and_then(Path::parent)
        .expect("workspace root from CARGO_MANIFEST_DIR")
        .to_path_buf();
    workspace_root.join("target/deploy/global_accountant.so")
}

/// Hex-encode a byte slice. Avoids a dev-dep on `hex`.
fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}

/// POST a JSON-RPC request directly via `ureq`-equivalent stdlib path. We don't
/// pull in a JSON-RPC dependency for the cheatcode; `solana-client` doesn't
/// expose `surfnet_*` methods.
fn rpc_call(url: &str, method: &str, params: serde_json::Value) -> serde_json::Value {
    use std::io::Write;
    use std::net::TcpStream;

    // `solana-client` ships a reqwest-based async client; calling it sync from
    // a test would require pulling tokio. The stdlib HTTP-1.1 path is fine for
    // a single localhost POST.
    let parsed = url::parse(url).expect("parse RPC URL");
    let host = parsed.host_str().expect("host").to_string();
    let port = parsed.port_or_known_default().expect("port");
    let body = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": method,
        "params": params,
    })
    .to_string();

    let mut stream = TcpStream::connect((host.as_str(), port)).expect("connect RPC");
    let request = format!(
        "POST / HTTP/1.1\r\nHost: {host}:{port}\r\nContent-Type: application/json\r\n\
         Content-Length: {len}\r\nConnection: close\r\n\r\n{body}",
        len = body.len()
    );
    stream.write_all(request.as_bytes()).expect("write HTTP");

    let mut response = Vec::new();
    stream.read_to_end(&mut response).expect("read HTTP");
    let response = String::from_utf8_lossy(&response);
    let body_start = response
        .find("\r\n\r\n")
        .expect("HTTP body separator")
        + 4;
    let raw_body = &response[body_start..];

    // The body may be chunked or plain. Localhost surfpool responds with
    // `Connection: close` + plain body when we request it; fall back to
    // permissive parsing if it adds chunking.
    if let Ok(v) = serde_json::from_str(raw_body) {
        return v;
    }
    // Crude chunked-decode: skip leading hex length line.
    if let Some(nl) = raw_body.find("\r\n") {
        let rest = &raw_body[nl + 2..];
        // Strip the trailing "\r\n0\r\n\r\n".
        let end = rest.find("\r\n").unwrap_or(rest.len());
        if let Ok(v) = serde_json::from_str(&rest[..end]) {
            return v;
        }
    }
    panic!("non-JSON RPC response from {method}: {raw_body}");
}

mod url {
    /// Minimal `http://host:port/...` parser. Real URL parsing is overkill for
    /// the single localhost RPC endpoint this test hits.
    pub struct Url {
        host: String,
        port: Option<u16>,
    }

    impl Url {
        pub fn host_str(&self) -> Option<&str> {
            Some(&self.host)
        }
        pub fn port_or_known_default(&self) -> Option<u16> {
            self.port.or(Some(80))
        }
    }

    pub fn parse(input: &str) -> Result<Url, &'static str> {
        let s = input
            .strip_prefix("http://")
            .ok_or("only http:// supported")?;
        let s = s.split('/').next().unwrap_or(s);
        let (host, port) = match s.rsplit_once(':') {
            Some((h, p)) => (h.to_string(), p.parse().ok()),
            None => (s.to_string(), None),
        };
        Ok(Url { host, port })
    }
}

/// Deploy the dev `.so` at `program_id` via `surfnet_writeProgram`.
fn deploy_program(rpc_url: &str, program_id: &Pubkey, so_bytes: &[u8]) {
    let hex = hex_encode(so_bytes);
    let resp = rpc_call(
        rpc_url,
        "surfnet_writeProgram",
        // Args: (program_id_b58, hex_data, slot). Slot 0 = "current".
        serde_json::json!([program_id.to_string(), hex, 0]),
    );
    assert!(
        resp.get("error").is_none(),
        "surfnet_writeProgram failed: {resp}"
    );
    eprintln!("[spike] writeProgram OK for {program_id}");
}

fn derive_digest_pda(
    program_id: &Pubkey,
    chain: u16,
    emitter: &[u8; 32],
    sequence: u64,
) -> (Pubkey, u8) {
    let chain_be = chain.to_be_bytes();
    let sequence_be = sequence.to_be_bytes();
    Pubkey::find_program_address(
        &[DIGEST_SEED_PREFIX, &chain_be, emitter, &sequence_be],
        program_id,
    )
}

fn open_digest_ix_data(
    chain: u16,
    emitter: &[u8; 32],
    sequence: u64,
    digest: &[u8; 32],
    guardian_set_index: u32,
    bump: u8,
) -> Vec<u8> {
    let mut data = Vec::with_capacity(1 + 79);
    data.push(IxDiscriminator::OpenDigest as u8);
    data.extend_from_slice(&chain.to_be_bytes());
    data.extend_from_slice(emitter);
    data.extend_from_slice(&sequence.to_be_bytes());
    data.extend_from_slice(digest);
    data.extend_from_slice(&guardian_set_index.to_le_bytes());
    data.push(bump);
    data
}

fn close_digest_ix_data(mock_vaa_digest: &[u8; 32]) -> Vec<u8> {
    // 32-byte digest + 1-byte guardian_set_bump. The bump is irrelevant for
    // the mock-vaa branch; the mainnet-fork e2e test populates the canonical
    // Shim guardian-set bump here.
    let mut data = Vec::with_capacity(1 + 32 + 1);
    data.push(IxDiscriminator::CloseDigest as u8);
    data.extend_from_slice(mock_vaa_digest);
    data.push(0);
    data
}

#[test]
#[ignore = "spawns surfpool subprocess; run via `make test-e2e-spike` or `cargo test -- --ignored`"]
fn surfpool_open_close_round_trip_spike() {
    // Locate the dev .so before spawning surfpool so a missing build fails fast
    // with a clear message rather than after a 30-second boot wait.
    let so = so_path();
    let so_bytes = std::fs::read(&so).unwrap_or_else(|e| {
        panic!(
            "could not read {}: {e}. Run `make build-dev` first.",
            so.display()
        )
    });
    eprintln!("[spike] loaded {} bytes from {}", so_bytes.len(), so.display());

    let guard = start_surfpool();
    let rpc_url = guard.rpc_url();
    let client = RpcClient::new_with_commitment(rpc_url.clone(), CommitmentConfig::confirmed());

    // Fresh per-run keys: ensures determinism on re-run within the same dev
    // machine (no stale on-disk state since `--offline` uses an in-memory DB).
    let program_keypair = Keypair::new();
    let program_id = program_keypair.pubkey();
    let payer = Keypair::new();
    eprintln!("[spike] program_id={program_id} payer={}", payer.pubkey());

    // Fund the payer via the validator's airdrop facility.
    let airdrop_sig = client
        .request_airdrop(&payer.pubkey(), 10_000_000_000)
        .expect("request_airdrop");
    // Wait for the airdrop to confirm; surfpool processes airdrops in the next
    // slot tick (100ms with our slot-time).
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match client.confirm_transaction(&airdrop_sig) {
            Ok(true) => break,
            Ok(false) => {}
            Err(e) => eprintln!("[spike] airdrop confirm wait: {e}"),
        }
        if Instant::now() > deadline {
            panic!("airdrop did not confirm in 10s");
        }
        thread::sleep(Duration::from_millis(100));
    }
    let payer_starting = client
        .get_balance(&payer.pubkey())
        .expect("payer balance after airdrop");
    assert!(payer_starting >= 10_000_000_000, "airdrop landed");

    // Deploy the program.
    deploy_program(&rpc_url, &program_id, &so_bytes);

    // Sanity: the program account exists, executable, BPF Loader-owned.
    let program_account = client
        .get_account(&program_id)
        .expect("program account after deploy");
    assert!(program_account.executable, "deployed program is executable");
    eprintln!(
        "[spike] program account: lamports={} owner={} space={}",
        program_account.lamports,
        program_account.owner,
        program_account.data.len()
    );

    // Compose the open_digest instruction.
    let chain: u16 = 1;
    let mut emitter = [0u8; 32];
    emitter[31] = 0x42;
    let sequence: u64 = 0x0123_4567_89ab_cdef;
    let mut digest = [0u8; 32];
    for (i, b) in digest.iter_mut().enumerate() {
        *b = i as u8;
    }
    let guardian_set_index: u32 = 3;

    let (pda, bump) = derive_digest_pda(&program_id, chain, &emitter, sequence);
    eprintln!("[spike] PDA={pda} bump={bump}");

    let open_data =
        open_digest_ix_data(chain, &emitter, sequence, &digest, guardian_set_index, bump);
    let open_ix = Instruction {
        program_id,
        accounts: vec![
            AccountMeta::new(payer.pubkey(), true),
            AccountMeta::new(pda, false),
            AccountMeta::new_readonly(system_program::ID, false),
        ],
        data: open_data,
    };

    let blockhash = client
        .get_latest_blockhash()
        .expect("blockhash for open tx");
    let open_tx = Transaction::new_signed_with_payer(
        &[open_ix],
        Some(&payer.pubkey()),
        &[&payer],
        blockhash,
    );
    let open_sig = client
        .send_and_confirm_transaction(&open_tx)
        .expect("open_digest send_and_confirm");
    eprintln!("[spike] open_digest tx={open_sig}");

    // Verify PDA exists with the expected layout.
    let pda_account = client.get_account(&pda).expect("PDA after open");
    assert_eq!(pda_account.owner, program_id, "PDA owner == program_id");
    assert_eq!(
        pda_account.data.len(),
        DigestAccountLayout::LEN,
        "PDA len == DigestAccountLayout::LEN"
    );
    let stored: &DigestAccountLayout = bytemuck::from_bytes(&pda_account.data);
    assert_eq!(stored.chain, chain);
    assert_eq!(stored.emitter, emitter);
    assert_eq!(stored.sequence, sequence);
    assert_eq!(stored.digest, digest);
    assert_eq!(stored.payer, payer.pubkey().to_bytes());
    assert_eq!(stored.guardian_set_index, guardian_set_index);
    assert_ne!(stored.quorum_at_slot, u64::MAX, "slot was written");

    let payer_after_open = client
        .get_balance(&payer.pubkey())
        .expect("payer balance after open");
    let rent_paid = payer_starting.saturating_sub(payer_after_open);
    assert!(rent_paid >= pda_account.lamports, "payer funded rent");
    eprintln!(
        "[spike] payer={} after_open={} rent_paid_incl_fee={} pda_lamports={}",
        payer_starting, payer_after_open, rent_paid, pda_account.lamports
    );

    // Close: mock-VAA path means the first 32 bytes of the VAA-bytes arg must
    // equal the stored digest. Under `mock-vaa` the three trailing accounts
    // (guardian-signatures PDA, guardian-set PDA, Verify VAA Shim program) are
    // accepted but ignored; the wire shape mirrors the production-shape build
    // so the mainnet-fork e2e test can drop in real Shim accounts without
    // changing the instruction builder.
    let close_data = close_digest_ix_data(&digest);
    let guardian_signatures_placeholder = Pubkey::new_from_array([0xE1; 32]);
    let guardian_set_placeholder = Pubkey::new_from_array([0xE2; 32]);
    let verify_vaa_shim_program = Pubkey::new_from_array(VERIFY_VAA_SHIM_PROGRAM_ID);
    let close_ix = Instruction {
        program_id,
        accounts: vec![
            AccountMeta::new_readonly(payer.pubkey(), true), // closer (signer)
            AccountMeta::new(pda, false),
            AccountMeta::new(payer.pubkey(), false), // rent recipient
            AccountMeta::new_readonly(guardian_signatures_placeholder, false),
            AccountMeta::new_readonly(guardian_set_placeholder, false),
            AccountMeta::new_readonly(verify_vaa_shim_program, false),
        ],
        data: close_data,
    };

    let blockhash = client
        .get_latest_blockhash()
        .expect("blockhash for close tx");
    let close_tx = Transaction::new_signed_with_payer(
        &[close_ix],
        Some(&payer.pubkey()),
        &[&payer],
        blockhash,
    );
    let close_sig = client
        .send_and_confirm_transaction(&close_tx)
        .expect("close_digest send_and_confirm");
    eprintln!("[spike] close_digest tx={close_sig}");

    // PDA should now be back in System-Program-owned, zero-data, zero-lamport
    // state. The simnet may report `getAccount` as `null` once an account has
    // been fully closed; treat that as the close-succeeded oracle.
    match client.get_account_with_commitment(&pda, CommitmentConfig::confirmed()) {
        Ok(resp) => match resp.value {
            None => {
                eprintln!("[spike] PDA fully closed (account does not exist)");
            }
            Some(acct) => {
                assert_eq!(acct.lamports, 0, "PDA lamports drained");
                assert_eq!(acct.owner, system_program::ID, "PDA reassigned to system");
                assert!(acct.data.is_empty(), "PDA data dropped");
            }
        },
        Err(e) => panic!("get_account after close: {e}"),
    }

    let payer_after_close = client
        .get_balance(&payer.pubkey())
        .expect("payer balance after close");
    assert!(
        payer_after_close > payer_after_open,
        "rent flowed back to payer: before={payer_after_open} after={payer_after_close}"
    );
    eprintln!(
        "[spike] payer balance: start={payer_starting} after_open={payer_after_open} \
         after_close={payer_after_close}"
    );

    // SurfpoolGuard's Drop cleans up the subprocess.
}
