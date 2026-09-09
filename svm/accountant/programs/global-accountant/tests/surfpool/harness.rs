//! Surfpool process control and RPC helpers shared by the e2e modules.

use std::io::{BufRead, BufReader};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use accountant_test_fixtures::{NOREPLAY_SO, VERIFY_VAA_SHIM_SO};
use global_accountant_definitions::{
    AccountantDigestLog, GlobalAccountantError, ACCOUNTANT_DIGEST_LOG_TAG,
};
use solana_account::Account;
use solana_client::rpc_client::RpcClient;
use solana_client::rpc_config::{RpcTransactionConfig, UiTransactionEncoding};
use solana_client::rpc_request::RpcRequest;
use solana_commitment_config::CommitmentConfig;
use solana_instruction::Instruction;
use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use solana_signature::Signature;
use solana_signer::Signer;
use solana_transaction::Transaction;

use crate::common::{fixture_elf, shim_program_id, NOREPLAY_PROGRAM_ID};

const SURFPOOL_BOOT_TIMEOUT: Duration = Duration::from_secs(45);
const RPC_READY_POLL_INTERVAL: Duration = Duration::from_millis(250);
const TX_INDEX_TIMEOUT: Duration = Duration::from_secs(5);
const TX_INDEX_POLL_INTERVAL: Duration = Duration::from_millis(150);

/// Launch options for one surfpool instance.
///
/// `datasource_rpc_url` selects the ledger mode. `None` starts an empty
/// offline ledger. `Some(url)` starts a fork: surfpool fetches each account
/// from that RPC on first access and caches it locally.
///
/// `scratch_prefix` names the working directory under the OS temp dir.
pub struct SurfpoolOptions {
    pub datasource_rpc_url: Option<String>,
    pub scratch_prefix: &'static str,
    /// Log to files in the scratch dir instead of piping into this process,
    /// so the child survives this process exiting. Pair with [`SurfpoolGuard::detach`].
    pub detached: bool,
}

impl SurfpoolOptions {
    pub fn offline(scratch_prefix: &'static str) -> Self {
        Self {
            datasource_rpc_url: None,
            scratch_prefix,
            detached: false,
        }
    }

    pub fn offline_detached(scratch_prefix: &'static str) -> Self {
        Self {
            datasource_rpc_url: None,
            scratch_prefix,
            detached: true,
        }
    }

    pub fn mainnet_fork(scratch_prefix: &'static str, rpc_url: impl Into<String>) -> Self {
        Self {
            datasource_rpc_url: Some(rpc_url.into()),
            scratch_prefix,
            detached: false,
        }
    }
}

/// Handle to a running surfpool child process.
///
/// `Drop` kills the child, so the instance lives as long as the guard. Call
/// [`SurfpoolGuard::detach`] to keep the instance alive after the test exits.
/// The pump threads copy the child's stdout and stderr into this process.
pub struct SurfpoolGuard {
    child: Child,
    rpc_port: u16,
    _stdout_pump: Option<thread::JoinHandle<()>>,
    _stderr_pump: Option<thread::JoinHandle<()>>,
}

impl SurfpoolGuard {
    pub fn rpc_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.rpc_port)
    }

    pub fn rpc_client(&self) -> RpcClient {
        rpc_client(&self.rpc_url())
    }

    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    /// Leave surfpool running after this process exits. Only sound with
    /// [`SurfpoolOptions::detached`], which sends the child's logs to files.
    pub fn detach(self) {
        std::mem::forget(self);
    }
}

impl Drop for SurfpoolGuard {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// RPC client at `confirmed` commitment, the level every helper here reads at.
pub fn rpc_client(url: &str) -> RpcClient {
    RpcClient::new_with_commitment(url.to_string(), CommitmentConfig::confirmed())
}

/// Ask the OS for a free TCP port on loopback.
///
/// Binds port 0, reads the assigned port, then drops the listener. Each call
/// gives a distinct port, so parallel instances start on different ports.
/// The port is free again before surfpool binds it; another process can take
/// it in that window. The boot check in [`start_surfpool`] catches that case.
pub fn free_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral");
    listener.local_addr().expect("local_addr").port()
}

/// Locate the `surfpool` executable.
///
/// Search order: each `PATH` entry, then `~/.local/bin/surfpool`, the path
/// the official install script uses. Panics with the install command when
/// both fail.
pub fn surfpool_binary() -> PathBuf {
    if let Ok(found) = which_global("surfpool") {
        return found;
    }
    let home = std::env::var("HOME").unwrap_or_default();
    let candidate = PathBuf::from(home).join(".local/bin/surfpool");
    if candidate.exists() {
        return candidate;
    }
    panic!("`surfpool` not on PATH and not at ~/.local/bin/surfpool; install: curl -sL https://run.surfpool.run/ | bash");
}

fn which_global(name: &str) -> Result<PathBuf, ()> {
    let path = std::env::var_os("PATH").ok_or(())?;
    for entry in std::env::split_paths(&path) {
        let candidate = entry.join(name);
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    Err(())
}

/// Start one surfpool instance and block until its RPC answers `getHealth`.
///
/// Steps:
/// 1. Locate the binary and reserve three loopback ports: RPC, WebSocket,
///    and studio.
/// 2. Recreate the scratch directory `<tmp>/<scratch_prefix>-<rpc_port>` and
///    make it the child's working directory. Surfpool writes its workspace
///    there.
/// 3. Spawn `surfpool start` headless with `--no-tui --no-studio`. The pair
///    `--no-deploy -y` skips the Anchor workspace scan and its prompts; tests
///    load programs with [`deploy_programs`] instead. `--slot-time 100` sets a
///    100 ms slot, so a transaction confirms in well under a second.
/// 4. Select the ledger mode: `--offline` for an empty ledger, or `--rpc-url`
///    to fork the datasource.
/// 5. Route child output. Attached mode pipes stdout and stderr into two pump
///    threads. Each pump echoes every line to this process's stderr with a
///    `[surfpool]` prefix and keeps its pipe drained; a full pipe blocks
///    surfpool. Detached mode writes both streams to files in the scratch
///    directory, so the child survives this process.
/// 6. Poll `getHealth` every 250 ms for up to 45 s, then return the guard.
///
/// Panics when the binary is missing, the spawn fails, or the RPC stays down
/// past the boot timeout. The guard kills the child on drop.
pub fn start_surfpool(opts: SurfpoolOptions) -> SurfpoolGuard {
    let bin = surfpool_binary();
    let rpc_port = free_port();
    let ws_port = free_port();
    let studio_port = free_port();

    let scratch = std::env::temp_dir().join(format!("{}-{}", opts.scratch_prefix, rpc_port));
    let _ = std::fs::remove_dir_all(&scratch);
    std::fs::create_dir_all(&scratch).expect("create scratch dir");

    eprintln!(
        "[surfpool] starting: bin={} rpc={} ws={} studio={} cwd={} datasource={:?}",
        bin.display(),
        rpc_port,
        ws_port,
        studio_port,
        scratch.display(),
        opts.datasource_rpc_url,
    );

    let mut cmd = Command::new(&bin);
    cmd.arg("start")
        .arg("--no-tui")
        .arg("--no-studio")
        .arg("--no-deploy")
        .arg("-y")
        .arg("--port")
        .arg(rpc_port.to_string())
        .arg("--ws-port")
        .arg(ws_port.to_string())
        .arg("--studio-port")
        .arg(studio_port.to_string())
        .arg("--slot-time")
        .arg("100")
        .arg("--log-level")
        .arg("warn");

    match &opts.datasource_rpc_url {
        Some(url) => {
            cmd.arg("--rpc-url").arg(url);
        }
        None => {
            cmd.arg("--offline");
        }
    }

    if opts.detached {
        let stdout_log =
            std::fs::File::create(scratch.join("surfpool.stdout.log")).expect("stdout log file");
        let stderr_log =
            std::fs::File::create(scratch.join("surfpool.stderr.log")).expect("stderr log file");
        cmd.current_dir(&scratch)
            .stdout(Stdio::from(stdout_log))
            .stderr(Stdio::from(stderr_log));
        eprintln!(
            "[surfpool] detached logs: {}/surfpool.{{stdout,stderr}}.log",
            scratch.display()
        );
    } else {
        cmd.current_dir(&scratch)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
    }

    let mut child = cmd.spawn().expect("spawn surfpool");

    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let stdout_pump = stdout.map(|s| {
        thread::spawn(move || {
            for line in BufReader::new(s).lines().map_while(Result::ok) {
                eprintln!("[surfpool stdout] {line}");
            }
        })
    });
    let stderr_pump = stderr.map(|s| {
        thread::spawn(move || {
            for line in BufReader::new(s).lines().map_while(Result::ok) {
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

/// Poll `getHealth` until it succeeds or `SURFPOOL_BOOT_TIMEOUT` passes.
///
/// Panics with the last RPC error on timeout. A refused connection is the
/// normal state during the first second of boot.
fn wait_for_rpc_ready(guard: &SurfpoolGuard) {
    let client = guard.rpc_client();
    let deadline = Instant::now() + SURFPOOL_BOOT_TIMEOUT;
    let mut last_err: Option<String> = None;
    while Instant::now() < deadline {
        match client.get_health() {
            Ok(_) => {
                eprintln!("[surfpool] RPC ready at {}", guard.rpc_url());
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

/// Path of a built program: `<workspace>/target/deploy/<name>.so`.
///
/// The workspace root is two levels above this crate's manifest directory.
/// Run `just build` first; the file must exist.
pub fn so_path(name: &str) -> PathBuf {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = manifest_dir
        .parent()
        .and_then(Path::parent)
        .expect("workspace root from CARGO_MANIFEST_DIR")
        .to_path_buf();
    workspace_root
        .join("target/deploy")
        .join(format!("{name}.so"))
}

/// A program to load into surfpool: its address and ELF bytes.
pub struct ProgramImage {
    pub label: &'static str,
    pub program_id: Pubkey,
    pub elf: Vec<u8>,
}

impl ProgramImage {
    /// The accountant from `target/deploy`, at its `declare_id!` address.
    pub fn accountant() -> Self {
        let path = so_path("global_accountant");
        let elf = std::fs::read(&path)
            .unwrap_or_else(|e| panic!("read {}: {e}. Run `just build` first.", path.display()));
        Self {
            label: "global_accountant",
            program_id: Pubkey::new_from_array(global_accountant::ID.to_bytes()),
            elf,
        }
    }

    /// NoReplay from the pinned fixture; `GA_NOREPLAY_SO` overrides.
    pub fn noreplay() -> Self {
        Self {
            label: "solana_noreplay",
            program_id: NOREPLAY_PROGRAM_ID,
            elf: fixture_elf(&NOREPLAY_SO, "solana_noreplay", "GA_NOREPLAY_SO"),
        }
    }

    /// Verify VAA Shim from the pinned fixture; `GA_VERIFY_VAA_SHIM_SO` overrides.
    pub fn verify_vaa_shim() -> Self {
        Self {
            label: "wormhole_verify_vaa_shim",
            program_id: shim_program_id(),
            elf: fixture_elf(
                &VERIFY_VAA_SHIM_SO,
                "wormhole_verify_vaa_shim",
                "GA_VERIFY_VAA_SHIM_SO",
            ),
        }
    }
}

/// Lowercase hex, two chars per byte. Surfpool cheat codes take program and
/// account data in this form.
pub fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}

/// Call a surfpool cheat code (`surfnet_*`) and return its JSON result.
///
/// `RpcClient::send` accepts any method name through `RpcRequest::Custom`, so
/// cheat codes share the client, transport, and error type of standard calls.
/// Panics on transport failure or a JSON-RPC error.
pub fn cheat_code(
    rpc: &RpcClient,
    method: &'static str,
    params: serde_json::Value,
) -> serde_json::Value {
    rpc.send::<serde_json::Value>(RpcRequest::Custom { method }, params)
        .unwrap_or_else(|e| panic!("{method} failed: {e}"))
}

/// Load each image as an executable program account via `surfnet_writeProgram`.
///
/// The whole ELF goes in one write at offset 0. Surfpool writes the program
/// and program-data accounts directly.
pub fn deploy_programs(rpc: &RpcClient, images: &[ProgramImage]) {
    assert!(!images.is_empty(), "deploy_programs called with no images");
    for image in images {
        cheat_code(
            rpc,
            "surfnet_writeProgram",
            serde_json::json!([image.program_id.to_string(), hex_encode(&image.elf), 0]),
        );
        eprintln!(
            "[surfpool] deployed {} ({} bytes) at {}",
            image.label,
            image.elf.len(),
            image.program_id
        );
    }
}

/// Write `account` at `key` via `surfnet_setAccount`, replacing any existing account.
pub fn set_account(rpc: &RpcClient, key: &Pubkey, account: &Account) {
    cheat_code(
        rpc,
        "surfnet_setAccount",
        serde_json::json!([
            key.to_string(),
            {
                "lamports": account.lamports,
                "owner": account.owner.to_string(),
                "executable": account.executable,
                "rent_epoch": account.rent_epoch,
                "data": hex_encode(&account.data),
            }
        ]),
    );
}

/// Airdrop `lamports` to `key` and wait for the transfer to confirm.
pub fn fund(rpc: &RpcClient, key: &Pubkey, lamports: u64) {
    let sig = rpc
        .request_airdrop(key, lamports)
        .unwrap_or_else(|e| panic!("airdrop {key}: {e}"));
    rpc.poll_for_signature(&sig)
        .unwrap_or_else(|e| panic!("airdrop {key} confirm: {e}"));
}

/// Sign `ixs` with `signers` (first signer pays), send, and wait for confirmation.
///
/// Panics with `label` when the transaction fails preflight or execution.
pub fn send(rpc: &RpcClient, label: &str, ixs: &[Instruction], signers: &[&Keypair]) -> Signature {
    let payer = signers.first().expect("at least one signer").pubkey();
    let blockhash = rpc
        .get_latest_blockhash()
        .unwrap_or_else(|e| panic!("{label} blockhash: {e}"));
    let tx = Transaction::new_signed_with_payer(ixs, Some(&payer), signers, blockhash);
    let sig = rpc
        .send_and_confirm_transaction(&tx)
        .unwrap_or_else(|e| panic!("{label} send_and_confirm: {e}"));
    eprintln!("[surfpool] {label} tx={sig}");
    sig
}

/// Send `ixs` and require the accountant to reject with `expected`.
///
/// Panics when the transaction confirms, or when it fails with any other error.
pub fn send_expect_error(
    rpc: &RpcClient,
    label: &str,
    ixs: &[Instruction],
    signers: &[&Keypair],
    expected: GlobalAccountantError,
) {
    let payer = signers.first().expect("at least one signer").pubkey();
    let blockhash = rpc
        .get_latest_blockhash()
        .unwrap_or_else(|e| panic!("{label} blockhash: {e}"));
    let tx = Transaction::new_signed_with_payer(ixs, Some(&payer), signers, blockhash);
    let err = rpc
        .send_and_confirm_transaction(&tx)
        .expect_err(&format!(
            "{label}: expected error {expected:?}, but tx confirmed"
        ))
        .get_transaction_error()
        .unwrap_or_else(|| panic!("{label}: failed before execution, not with {expected:?}"));
    // `TransactionError` is not re-exported by `solana_client`, so match its rendering:
    // "Error processing Instruction N: custom program error: 0x<code>".
    let rendered = err.to_string();
    let code = format!("custom program error: {:#x}", expected as u64);
    assert!(
        rendered.contains(&code),
        "{label}: expected {expected:?} ({code}), got: {rendered}"
    );
}

/// Assert that transaction `sig` emitted exactly one accountant commit-log
/// record with the expected fields.
///
/// Polls `getTransaction` for up to 5 s, because surfpool indexes a confirmed
/// transaction a little after confirmation. Then scans the log messages for
/// `Program data: <base64>` lines whose decoded bytes start with
/// `ACCOUNTANT_DIGEST_LOG_TAG`. The scan ignores other `Program data` lines.
/// The count must equal 1, so a duplicate commit and a missing commit both
/// fail.
pub fn assert_canonical_log_in_tx(
    rpc: &RpcClient,
    sig: &Signature,
    expected_chain: u16,
    expected_emitter: &[u8; 32],
    expected_sequence: u64,
    expected_digest: &[u8; 32],
    expected_guardian_set_index: u32,
) {
    use base64::Engine;

    let config = RpcTransactionConfig {
        encoding: Some(UiTransactionEncoding::Json),
        commitment: Some(CommitmentConfig::confirmed()),
        max_supported_transaction_version: Some(0),
    };
    let deadline = Instant::now() + TX_INDEX_TIMEOUT;
    let confirmed = loop {
        match rpc.get_transaction_with_config(sig, config) {
            Ok(tx) => break tx,
            Err(e) if Instant::now() < deadline => {
                let _ = e;
                thread::sleep(TX_INDEX_POLL_INTERVAL);
            }
            Err(e) => panic!("getTransaction {sig} not indexed within {TX_INDEX_TIMEOUT:?}: {e}"),
        }
    };
    let meta = confirmed
        .transaction
        .meta
        .unwrap_or_else(|| panic!("getTransaction {sig} returned no meta"));
    let logs: Option<Vec<String>> = meta.log_messages.into();
    let logs = logs.unwrap_or_else(|| panic!("getTransaction {sig} returned no logMessages"));

    let mut matched = 0usize;
    for line in &logs {
        let Some(b64) = line.strip_prefix("Program data: ") else {
            continue;
        };
        let bytes = match base64::engine::general_purpose::STANDARD.decode(b64.trim()) {
            Ok(b) => b,
            Err(_) => continue,
        };
        if bytes.len() < 8 || bytes[..8] != ACCOUNTANT_DIGEST_LOG_TAG {
            continue;
        }
        let entry = AccountantDigestLog::from_bytes(&bytes)
            .unwrap_or_else(|| panic!("commit-log payload malformed: {} bytes", bytes.len()));
        assert_eq!(entry.chain(), expected_chain, "commit-log chain");
        assert_eq!(&entry.emitter, expected_emitter, "commit-log emitter");
        assert_eq!(entry.sequence(), expected_sequence, "commit-log sequence");
        assert_eq!(&entry.digest, expected_digest, "commit-log digest");
        assert_eq!(
            entry.guardian_set_index(),
            expected_guardian_set_index,
            "commit-log guardian_set_index"
        );
        matched += 1;
    }
    assert_eq!(matched, 1, "exactly one canonical commit-log entry");
}
