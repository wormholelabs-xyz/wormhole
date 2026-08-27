//! Shared test helpers: surfpool process management, cheatcodes, and mollusk fixtures.

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

const SURFPOOL_BOOT_TIMEOUT: Duration = Duration::from_secs(45);
const RPC_READY_POLL_INTERVAL: Duration = Duration::from_millis(250);

/// Surfpool options. `datasource_rpc_url = None` boots `--offline`; `Some(url)` forks mainnet.
pub struct SurfpoolOptions {
    pub datasource_rpc_url: Option<String>,
    /// Scratch dir prefix for surfpool artefacts.
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

/// Owns the surfpool child and its drain threads; `Drop` kills the child.
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
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Free TCP port.
pub fn free_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral");
    listener.local_addr().expect("local_addr").port()
}

/// Resolve the `surfpool` binary; falls back to `~/.local/bin/surfpool`.
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

/// Boot surfpool and block until JSON-RPC is healthy.
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

    // Drain pipes so surfpool writes do not block.
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

/// Path to the SBF `.so` under `target/deploy/`. Build first.
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

/// `solana_noreplay.so`: the pinned fixture, or `GA_NOREPLAY_SO` (unchecked).
pub fn noreplay_so_path() -> PathBuf {
    match std::env::var("GA_NOREPLAY_SO") {
        Ok(p) => PathBuf::from(p),
        Err(_) => accountant_test_fixtures::NOREPLAY_SO.path().to_path_buf(),
    }
}

/// `solana-noreplay` program ID, same compile-time source as the program under test.
pub const NOREPLAY_PROGRAM_ID: Pubkey =
    Pubkey::new_from_array(global_accountant_definitions::NOREPLAY_PROGRAM_ID);

/// NoReplay bitmap PDA for `(authority, namespace, sequence)`. Seeds:
/// `[authority, namespace[..min(len, 32)], namespace[min(len, 32)..], (sequence / 1024) LE]`.
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

/// Hex-encode.
pub fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}

/// POST a JSON-RPC request to surfpool over plain HTTP/1.1. Localhost only.
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
    // Chunked decode.
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

/// Deploy a `.so` at `program_id` through `surfnet_writeProgram`.
pub fn deploy_program(rpc_url: &str, program_id: &Pubkey, so_bytes: &[u8]) {
    let hex = hex_encode(so_bytes);
    let resp = rpc_call(
        rpc_url,
        "surfnet_writeProgram",
        // Args: (program_id_b58, hex_data, slot=0 = current).
        serde_json::json!([program_id.to_string(), hex, 0]),
    );
    assert!(
        resp.get("error").is_none(),
        "surfnet_writeProgram failed: {resp}"
    );
    eprintln!("[surfpool] writeProgram OK for {program_id}");
}

/// Parsed Wormhole VAA v1 fixture.
/// Wire format:
///
/// | offset | size  | field                                      |
/// |--------|-------|--------------------------------------------|
/// | 0      | 1     | version (always 1)                         |
/// | 1      | 4     | guardian_set_index (big-endian)            |
/// | 5      | 1     | num_signatures                             |
/// | 6      | 66*N  | signatures (1-byte index + 64-byte r\|\|s + 1-byte recovery_id), strictly increasing by index |
/// | 6+66N  | 4     | timestamp                                  |
/// | ...    | 4     | nonce                                      |
/// | ...    | 2     | emitter_chain                              |
/// | ...    | 32    | emitter_address                            |
/// | ...    | 8     | sequence                                   |
/// | ...    | 1     | consistency_level                          |
/// | ...    | rest  | payload                                    |
///
/// `digest` is `keccak256(keccak256(body))`.
#[derive(Clone)]
pub struct ParsedVaa {
    /// Raw VAA bytes.
    pub bytes: Vec<u8>,
    /// `keccak256(keccak256(body))`.
    pub digest: [u8; 32],
    pub guardian_set_index: u32,
    pub num_signatures: u8,
    /// Signature block offset (6 for v1).
    pub signatures_offset: usize,
    /// Body offset (`6 + 66 * num_signatures`).
    pub body_offset: usize,
    pub emitter_chain: u16,
    pub emitter_address: [u8; 32],
    pub sequence: u64,
    pub payload_len: usize,
}

impl ParsedVaa {
    /// One signature record: index (1) + r||s (64) + recovery_id (1).
    pub const GUARDIAN_SIGNATURE_LENGTH: usize = 66;

    /// Signature block (`num_signatures * 66` bytes).
    pub fn signatures_slice(&self) -> &[u8] {
        let start = self.signatures_offset;
        let end = start + (self.num_signatures as usize) * Self::GUARDIAN_SIGNATURE_LENGTH;
        &self.bytes[start..end]
    }
}

/// Parse an embedded VAA fixture. Panics on a bad format.
pub fn load_vaa_fixture(vaa: &accountant_test_fixtures::Vaa) -> ParsedVaa {
    parse_vaa(vaa.bytes).unwrap_or_else(|e| panic!("parse VAA fixture: {e}"))
}

/// Pure parser for in-memory blobs.
pub fn parse_vaa(bytes: &[u8]) -> Result<ParsedVaa, String> {
    if bytes.len() < 6 {
        return Err(format!("VAA too short: {} bytes", bytes.len()));
    }
    let version = bytes[0];
    if version != 1 {
        return Err(format!("unsupported VAA version: {version}"));
    }
    let guardian_set_index = u32::from_be_bytes([bytes[1], bytes[2], bytes[3], bytes[4]]);
    let num_signatures = bytes[5];
    let signatures_offset: usize = 6;
    let body_offset =
        signatures_offset + (num_signatures as usize) * ParsedVaa::GUARDIAN_SIGNATURE_LENGTH;
    if bytes.len() < body_offset + 4 + 4 + 2 + 32 + 8 + 1 {
        return Err(format!(
            "VAA truncated before body end: total={}, body_offset={}",
            bytes.len(),
            body_offset
        ));
    }
    let body = &bytes[body_offset..];
    // body: timestamp(4) nonce(4) emitter_chain(2) emitter(32) sequence(8) consistency(1) payload
    let emitter_chain = u16::from_be_bytes([body[8], body[9]]);
    let mut emitter_address = [0u8; 32];
    emitter_address.copy_from_slice(&body[10..42]);
    let sequence = u64::from_be_bytes([
        body[42], body[43], body[44], body[45], body[46], body[47], body[48], body[49],
    ]);
    let payload_len = body.len().saturating_sub(51);
    let digest = double_keccak(body);
    Ok(ParsedVaa {
        bytes: bytes.to_vec(),
        digest,
        guardian_set_index,
        num_signatures,
        signatures_offset,
        body_offset,
        emitter_chain,
        emitter_address,
        sequence,
        payload_len,
    })
}

/// `keccak256(keccak256(body))`.
fn double_keccak(body: &[u8]) -> [u8; 32] {
    let inner = solana_keccak_hasher::hash(body);
    let outer = solana_keccak_hasher::hashv(&[&inner.to_bytes()]);
    outer.to_bytes()
}

/// Fetch the logs for `tx_sig`. Assert exactly one `Program data:` line decodes to the
/// `commit_log::emit` payload (`AccountantDigestLog`).
pub fn assert_canonical_log_in_tx(
    rpc_url: &str,
    tx_sig: &str,
    expected_chain: u16,
    expected_emitter: &[u8; 32],
    expected_sequence: u64,
    expected_digest: &[u8; 32],
    expected_guardian_set_index: u32,
) {
    use base64::Engine;
    use global_accountant_definitions::{AccountantDigestLog, ACCOUNTANT_DIGEST_LOG_TAG};

    // Indexing can lag a slot.
    let mut last_resp = serde_json::Value::Null;
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        last_resp = rpc_call(
            rpc_url,
            "getTransaction",
            serde_json::json!([
                tx_sig,
                {
                    "encoding": "json",
                    "commitment": "confirmed",
                    "maxSupportedTransactionVersion": 0,
                }
            ]),
        );
        if last_resp.get("result").is_some_and(|v| !v.is_null()) {
            break;
        }
        thread::sleep(Duration::from_millis(150));
    }

    let logs = last_resp
        .get("result")
        .and_then(|r| r.get("meta"))
        .and_then(|m| m.get("logMessages"))
        .and_then(|l| l.as_array())
        .unwrap_or_else(|| panic!("getTransaction returned no meta.logMessages: {last_resp}"));

    let mut matched = 0usize;
    for entry in logs {
        let line = entry.as_str().unwrap_or("");
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

        assert_eq!(entry.chain(), expected_chain, "commit-log chain mismatch");
        assert_eq!(
            &entry.emitter, expected_emitter,
            "commit-log emitter mismatch"
        );
        assert_eq!(
            entry.sequence(),
            expected_sequence,
            "commit-log sequence mismatch"
        );
        assert_eq!(&entry.digest, expected_digest, "commit-log digest mismatch");
        assert_eq!(
            entry.guardian_set_index(),
            expected_guardian_set_index,
            "commit-log guardian_set_index mismatch"
        );
        matched += 1;
    }

    assert_eq!(
        matched, 1,
        "expected exactly one canonical commit-log entry, found {matched}"
    );
}

/// Poll `confirm_transaction` until `Ok(true)` or timeout.
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
