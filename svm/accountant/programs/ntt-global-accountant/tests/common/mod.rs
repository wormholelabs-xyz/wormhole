//! Shared mollusk fixtures for the NTT operational program's integration tests,
//! plus surfpool subprocess management and cheatcode plumbing for the e2e
//! suite (ported from `programs/global-accountant/tests/common/mod.rs` — this
//! infra is product-neutral).
//!
//! The sibling `.so` programs CPI'd into during a test (`solana_noreplay`,
//! `wormhole_verify_vaa_shim`) are the same hash-pinned artefacts the WTT
//! suite uses, embedded via the shared `accountant-test-fixtures` crate.
//! Local iteration can redirect each via `GA_NOREPLAY_SO` /
//! `GA_VERIFY_VAA_SHIM_SO`.

#![allow(dead_code)] // Different integration tests use different subsets.

pub mod guardian_fixtures;
pub mod mollusk_fixtures;

use std::{
    io::{BufRead, BufReader, Read},
    net::TcpListener,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use solana_client::rpc_client::RpcClient;
use solana_commitment_config::CommitmentConfig;
use solana_pubkey::Pubkey;

/// Resolve `solana_noreplay.so`: the pinned `accountant-test-fixtures` copy by
/// default, or `GA_NOREPLAY_SO` for local iteration.
pub fn noreplay_so_path() -> PathBuf {
    match std::env::var("GA_NOREPLAY_SO") {
        Ok(p) => PathBuf::from(p),
        Err(_) => accountant_test_fixtures::NOREPLAY_SO.path().to_path_buf(),
    }
}

/// Resolve `wormhole_verify_vaa_shim.so`: the pinned `accountant-test-fixtures`
/// copy by default, or `GA_VERIFY_VAA_SHIM_SO` for local iteration.
pub fn verify_vaa_shim_so_path() -> PathBuf {
    match std::env::var("GA_VERIFY_VAA_SHIM_SO") {
        Ok(p) => PathBuf::from(p),
        Err(_) => accountant_test_fixtures::VERIFY_VAA_SHIM_SO
            .path()
            .to_path_buf(),
    }
}

// ============================================================================
// Surfpool subprocess management (product-neutral; ported from the WTT
// `global-accountant` crate's `tests/common/mod.rs`).
// ============================================================================

const SURFPOOL_BOOT_TIMEOUT: Duration = Duration::from_secs(45);
const RPC_READY_POLL_INTERVAL: Duration = Duration::from_millis(250);

/// Options for starting surfpool. `datasource_rpc_url = None` boots `--offline`;
/// `Some(url)` forks from mainnet.
pub struct SurfpoolOptions {
    pub datasource_rpc_url: Option<String>,
    /// Per-test scratch dir prefix; surfpool drops `.surfpool/` artefacts here.
    pub scratch_prefix: &'static str,
}

impl SurfpoolOptions {
    pub fn offline(scratch_prefix: &'static str) -> Self {
        Self {
            datasource_rpc_url: None,
            scratch_prefix,
        }
    }

    pub fn mainnet_fork(scratch_prefix: &'static str, rpc_url: impl Into<String>) -> Self {
        Self {
            datasource_rpc_url: Some(rpc_url.into()),
            scratch_prefix,
        }
    }
}

/// Owns the surfpool child process and its stdout/stderr drain threads;
/// `Drop` kills the child even on panic.
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
        RpcClient::new_with_commitment(self.rpc_url(), CommitmentConfig::confirmed())
    }
}

impl Drop for SurfpoolGuard {
    fn drop(&mut self) {
        // Best-effort SIGKILL; `wait` reaps the zombie.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Pick a free TCP port by binding to 0 and reading the assigned port.
pub fn free_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral");
    listener.local_addr().expect("local_addr").port()
}

/// Resolve the `surfpool` binary, falling back to `~/.local/bin/surfpool`
/// (the installer's default, not always on `$PATH` under `cargo test`).
pub fn surfpool_binary() -> PathBuf {
    if let Ok(found) = which_global("surfpool") {
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

/// Boot surfpool with the supplied options and block until JSON-RPC is healthy.
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

    cmd.current_dir(&scratch)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = cmd.spawn().expect("spawn surfpool");

    // Drain stdout/stderr into the test's stderr; required so the pipes do not
    // fill and block surfpool's writes.
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

/// Path to the SBF `.so` for the named program crate under `target/deploy/`.
/// The caller must have run the relevant build recipe (`just build`) first.
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

/// Canonical `solana-noreplay` program ID
/// (`repMHgR5BEpGLeZvM5iGoNNDPw4eu2BS6sXJzaC8K4t`), pinned as raw bytes to
/// avoid a base58 dev-dep.
pub const NOREPLAY_PROGRAM_ID: Pubkey = Pubkey::new_from_array([
    0x0c, 0xb8, 0x38, 0x00, 0x73, 0xdf, 0x36, 0x25, 0xa1, 0x32, 0x11, 0x1f, 0xee, 0x67, 0x8d, 0xd0,
    0x6b, 0x7e, 0x3d, 0xf2, 0x90, 0xa2, 0xb1, 0xd5, 0x4a, 0x48, 0x5b, 0xdb, 0x72, 0x61, 0x82, 0x91,
]);

/// Derive the solana-noreplay bitmap PDA for `(authority, namespace, sequence)`.
/// Mirrors `solana_noreplay::pda::BitmapPdaSeeds`:
///   seeds = [authority, namespace[..min(len, 32)], namespace[min(len, 32)..],
///            (sequence / 1024).to_le_bytes()]
pub fn derive_noreplay_bitmap_pda(
    authority: &Pubkey,
    namespace: &[u8],
    sequence: u64,
) -> (Pubkey, u8) {
    const BITS_PER_BUCKET: u64 = 1024; // BITMAP_BYTES (128) * 8
    const SEED_CHUNK_SIZE: usize = 32;
    let bucket_index = sequence / BITS_PER_BUCKET;
    let bucket_bytes = bucket_index.to_le_bytes();
    let mid = namespace.len().min(SEED_CHUNK_SIZE);
    let seeds: [&[u8]; 4] = [
        authority.as_ref(),
        &namespace[..mid],
        &namespace[mid..],
        &bucket_bytes,
    ];
    Pubkey::find_program_address(&seeds, &NOREPLAY_PROGRAM_ID)
}

/// Hex-encode a byte slice. Avoids a dev-dep on `hex`.
pub fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}

/// POST a JSON-RPC request to surfpool. Hand-rolled HTTP/1.1 because
/// `solana-client` does not expose the `surfnet_*` cheatcodes. Localhost only;
/// no TLS, no chunking, no keep-alive.
pub fn rpc_call(url: &str, method: &str, params: serde_json::Value) -> serde_json::Value {
    use std::io::Write;
    use std::net::TcpStream;

    let parsed = parse_url(url).expect("parse RPC URL");
    let host = parsed.host;
    let port = parsed.port.unwrap_or(80);
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
    let body_start = response.find("\r\n\r\n").expect("HTTP body separator") + 4;
    let raw_body = &response[body_start..];

    if let Ok(v) = serde_json::from_str(raw_body) {
        return v;
    }
    // Chunked-decode fallback.
    if let Some(nl) = raw_body.find("\r\n") {
        let rest = &raw_body[nl + 2..];
        let end = rest.find("\r\n").unwrap_or(rest.len());
        if let Ok(v) = serde_json::from_str(&rest[..end]) {
            return v;
        }
    }
    panic!("non-JSON RPC response from {method}: {raw_body}");
}

struct ParsedUrl {
    host: String,
    port: Option<u16>,
}

fn parse_url(input: &str) -> Result<ParsedUrl, &'static str> {
    let s = input
        .strip_prefix("http://")
        .ok_or("only http:// supported")?;
    let s = s.split('/').next().unwrap_or(s);
    let (host, port) = match s.rsplit_once(':') {
        Some((h, p)) => (h.to_string(), p.parse().ok()),
        None => (s.to_string(), None),
    };
    Ok(ParsedUrl { host, port })
}

/// Deploy a `.so` at `program_id` via the `surfnet_writeProgram` cheatcode.
pub fn deploy_program(rpc_url: &str, program_id: &Pubkey, so_bytes: &[u8]) {
    let hex = hex_encode(so_bytes);
    let resp = rpc_call(
        rpc_url,
        "surfnet_writeProgram",
        // Args: (program_id_b58, hex_data, slot=0 ⇒ current).
        serde_json::json!([program_id.to_string(), hex, 0]),
    );
    assert!(
        resp.get("error").is_none(),
        "surfnet_writeProgram failed: {resp}"
    );
    eprintln!("[surfpool] writeProgram OK for {program_id}");
}

/// Write an arbitrary account (owner + data) into the ledger via the
/// `surfnet_setAccount` cheatcode.
pub fn set_account(rpc_url: &str, pubkey: &Pubkey, owner: &Pubkey, lamports: u64, data: &[u8]) {
    let resp = rpc_call(
        rpc_url,
        "surfnet_setAccount",
        serde_json::json!([
            pubkey.to_string(),
            {
                "lamports": lamports,
                "owner": owner.to_string(),
                "executable": false,
                "rent_epoch": 0u64,
                "data": hex_encode(data),
            }
        ]),
    );
    assert!(
        resp.get("error").is_none(),
        "surfnet_setAccount failed for {pubkey}: {resp}"
    );
}

/// Poll `confirm_transaction` until `Ok(true)` or the timeout.
pub fn await_confirmed<F: Fn() -> Result<bool, solana_client::client_error::ClientError>>(
    label: &str,
    timeout: Duration,
    poll: F,
) {
    let deadline = Instant::now() + timeout;
    loop {
        match poll() {
            Ok(true) => return,
            Ok(false) => {}
            Err(e) => eprintln!("[surfpool] {label} confirm wait: {e}"),
        }
        if Instant::now() > deadline {
            panic!("{label} did not confirm in {timeout:?}");
        }
        thread::sleep(Duration::from_millis(100));
    }
}
