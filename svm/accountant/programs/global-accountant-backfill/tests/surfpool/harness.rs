//! Surfpool subprocess management for the backfill program's e2e tests.
//!
//! Lift the shared bits into a dedicated `crates/test-utils/` crate once
//! more programs join the workspace.

#![allow(dead_code)]

use std::{
    io::{BufRead, BufReader, Read, Write},
    net::{TcpListener, TcpStream},
    path::PathBuf,
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use solana_client::rpc_client::RpcClient;
use solana_commitment_config::CommitmentConfig;
use solana_pubkey::Pubkey;

const SURFPOOL_BOOT_TIMEOUT: Duration = Duration::from_secs(45);
const RPC_READY_POLL_INTERVAL: Duration = Duration::from_millis(250);

pub struct SurfpoolOptions {
    pub datasource_rpc_url: Option<String>,
    pub scratch_prefix: &'static str,
}

impl SurfpoolOptions {
    pub fn offline(scratch_prefix: &'static str) -> Self {
        Self {
            datasource_rpc_url: None,
            scratch_prefix,
        }
    }
}

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

pub fn free_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral");
    listener.local_addr().expect("local_addr").port()
}

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

pub fn start_surfpool(opts: SurfpoolOptions) -> SurfpoolGuard {
    let bin = surfpool_binary();
    let rpc_port = free_port();
    let ws_port = free_port();
    let studio_port = free_port();

    let scratch = std::env::temp_dir().join(format!("{}-{}", opts.scratch_prefix, rpc_port));
    let _ = std::fs::remove_dir_all(&scratch);
    std::fs::create_dir_all(&scratch).expect("create scratch dir");

    eprintln!(
        "[surfpool] starting: bin={} rpc={} ws={} datasource={:?}",
        bin.display(),
        rpc_port,
        ws_port,
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

pub fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}

pub fn rpc_call(url: &str, method: &str, params: serde_json::Value) -> serde_json::Value {
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

pub fn deploy_program(rpc_url: &str, program_id: &Pubkey, so_bytes: &[u8]) {
    let hex = hex_encode(so_bytes);
    let resp = rpc_call(
        rpc_url,
        "surfnet_writeProgram",
        serde_json::json!([program_id.to_string(), hex, 0]),
    );
    assert!(
        resp.get("error").is_none(),
        "surfnet_writeProgram failed: {resp}"
    );
    eprintln!("[surfpool] writeProgram OK for {program_id}");
}

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

/// One decoded `ACCDGST\0` entry: `(chain, emitter, sequence, digest,
/// guardian_set_index)`.
pub type AccdgstEntry = (u16, [u8; 32], u64, [u8; 32], u32);

/// Walk `meta.logMessages` for a tx, decode the canonical 86-byte
/// `ACCDGST\0` commit-log payloads, and return them in encounter order.
pub fn fetch_accdgst_logs(rpc_url: &str, tx_sig: &str) -> Vec<AccdgstEntry> {
    use base64::Engine;
    use global_accountant_definitions::AccountantDigestLog;

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

    let mut out = Vec::new();
    for entry in logs {
        let line = entry.as_str().unwrap_or("");
        let Some(b64) = line.strip_prefix("Program data: ") else {
            continue;
        };
        let bytes = match base64::engine::general_purpose::STANDARD.decode(b64.trim()) {
            Ok(b) => b,
            Err(_) => continue,
        };
        let Some(entry) = AccountantDigestLog::from_bytes(&bytes) else {
            continue;
        };
        out.push((
            entry.chain(),
            entry.emitter,
            entry.sequence(),
            entry.digest,
            entry.guardian_set_index(),
        ));
    }
    out
}
