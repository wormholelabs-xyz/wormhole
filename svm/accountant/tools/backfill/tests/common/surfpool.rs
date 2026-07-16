//! Spawn surfpool as a subprocess for e2e tests. Lifted from
//! `programs/global-accountant-backfill/tests/common/surfpool.rs` and
//! minimally adapted to the orchestrator's needs (we don't reuse the
//! source crate because test modules aren't accessible cross-crate).

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
    pub scratch_prefix: &'static str,
}

impl SurfpoolOptions {
    pub fn offline(scratch_prefix: &'static str) -> Self {
        Self { scratch_prefix }
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

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .expect("bind ephemeral")
        .local_addr()
        .expect("local_addr")
        .port()
}

fn surfpool_binary() -> PathBuf {
    if let Ok(path) = std::env::var("PATH") {
        for entry in std::env::split_paths(&path) {
            let candidate = entry.join("surfpool");
            if candidate.is_file() {
                return candidate;
            }
        }
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

pub fn start_surfpool(opts: SurfpoolOptions) -> SurfpoolGuard {
    let bin = surfpool_binary();
    let rpc_port = free_port();
    let ws_port = free_port();
    let studio_port = free_port();

    let scratch = std::env::temp_dir().join(format!("{}-{}", opts.scratch_prefix, rpc_port));
    let _ = std::fs::remove_dir_all(&scratch);
    std::fs::create_dir_all(&scratch).expect("create scratch dir");

    eprintln!(
        "[surfpool] bin={} rpc={rpc_port} cwd={}",
        bin.display(),
        scratch.display()
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
        .arg("warn")
        .arg("--offline")
        .current_dir(&scratch)
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
    while Instant::now() < deadline {
        if client.get_health().is_ok() {
            eprintln!("[surfpool] RPC ready at {}", guard.rpc_url());
            return;
        }
        thread::sleep(RPC_READY_POLL_INTERVAL);
    }
    panic!("surfpool RPC at {} not healthy in time", guard.rpc_url());
}

/// Hand-rolled JSON-RPC POST. `solana_client` doesn't expose `surfnet_*`
/// cheatcodes; we need raw access. Localhost only, no TLS, no keep-alive.
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
    let body_start = response.find("\r\n\r\n").expect("HTTP body sep") + 4;
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
    let s = input.strip_prefix("http://").ok_or("only http://")?;
    let s = s.split('/').next().unwrap_or(s);
    let (host, port) = match s.rsplit_once(':') {
        Some((h, p)) => (h.to_string(), p.parse().ok()),
        None => (s.to_string(), None),
    };
    Ok(ParsedUrl { host, port })
}

pub fn deploy_program(rpc_url: &str, program_id: &Pubkey, so_bytes: &[u8]) {
    let hex_data = hex::encode(so_bytes);
    let resp = rpc_call(
        rpc_url,
        "surfnet_writeProgram",
        serde_json::json!([program_id.to_string(), hex_data, 0]),
    );
    assert!(
        resp.get("error").is_none(),
        "surfnet_writeProgram failed: {resp}"
    );
    eprintln!("[surfpool] writeProgram OK for {program_id}");
}

/// Path to the parent workspace's SBF build output for the named program.
/// The orchestrator crate is a standalone workspace, so its own
/// `target/` is not the right place — the .so lives in the parent
/// `svm/accountant/target/deploy/`.
pub fn parent_so_path(name: &str) -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    // .../svm/accountant/tools/backfill → .../svm/accountant
    let parent_workspace = manifest
        .parent()
        .and_then(|p| p.parent())
        .expect("walk up to parent workspace");
    parent_workspace
        .join("target/deploy")
        .join(format!("{name}.so"))
}

pub fn noreplay_so_path() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let parent_workspace = manifest.parent().and_then(|p| p.parent()).expect("walk up");
    parent_workspace.join("programs/global-accountant/tests/fixtures/solana_noreplay.so")
}
