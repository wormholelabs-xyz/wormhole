//! Surfpool subprocess for the parity e2e: boot, `surfnet_setAccount`, teardown.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use solana_client::rpc_client::RpcClient;
use solana_commitment_config::CommitmentConfig;
use solana_pubkey::Pubkey;

const SURFPOOL_BOOT_TIMEOUT: Duration = Duration::from_secs(45);

pub struct SurfpoolGuard {
    child: Child,
    rpc_port: u16,
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
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral");
    listener.local_addr().expect("local_addr").port()
}

fn surfpool_binary() -> PathBuf {
    if let Some(path) = std::env::var_os("PATH") {
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
    panic!("`surfpool` not on PATH and not at ~/.local/bin/surfpool");
}

pub fn start_surfpool(scratch_prefix: &str) -> SurfpoolGuard {
    let rpc_port = free_port();
    let scratch = std::env::temp_dir().join(format!("{scratch_prefix}-{rpc_port}"));
    let _ = std::fs::remove_dir_all(&scratch);
    std::fs::create_dir_all(&scratch).expect("create scratch dir");

    let child = Command::new(surfpool_binary())
        .args([
            "start",
            "--no-tui",
            "--no-studio",
            "--no-deploy",
            "-y",
            "--offline",
        ])
        .args(["--port", &rpc_port.to_string()])
        .args(["--ws-port", &free_port().to_string()])
        .args(["--studio-port", &free_port().to_string()])
        .args(["--slot-time", "100", "--log-level", "warn"])
        .current_dir(&scratch)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn surfpool");

    let guard = SurfpoolGuard { child, rpc_port };
    let client = guard.rpc_client();
    let deadline = Instant::now() + SURFPOOL_BOOT_TIMEOUT;
    loop {
        if client.get_health().is_ok() {
            return guard;
        }
        assert!(
            Instant::now() < deadline,
            "surfpool RPC at {} not healthy within {SURFPOOL_BOOT_TIMEOUT:?}",
            guard.rpc_url(),
        );
        thread::sleep(Duration::from_millis(250));
    }
}

pub fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn rpc_call(url: &str, method: &str, params: serde_json::Value) -> serde_json::Value {
    let stripped = url.strip_prefix("http://").expect("http:// URL");
    let (host, port) = stripped.rsplit_once(':').expect("host:port");
    let port: u16 = port.parse().expect("port");
    let body = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": method,
        "params": params,
    })
    .to_string();

    let mut stream = TcpStream::connect((host, port)).expect("connect RPC");
    let request = format!(
        "POST / HTTP/1.1\r\nHost: {host}:{port}\r\nContent-Type: application/json\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len(),
    );
    stream.write_all(request.as_bytes()).expect("write HTTP");

    let mut response = Vec::new();
    stream.read_to_end(&mut response).expect("read HTTP");
    let response = String::from_utf8_lossy(&response);
    let body_start = response.find("\r\n\r\n").expect("HTTP body separator") + 4;
    let raw_body = &response[body_start..];
    if let Ok(value) = serde_json::from_str(raw_body) {
        return value;
    }
    // Chunked transfer: JSON sits between the chunk-size lines.
    if let Some(newline) = raw_body.find("\r\n") {
        let rest = &raw_body[newline + 2..];
        let end = rest.find("\r\n").unwrap_or(rest.len());
        if let Ok(value) = serde_json::from_str(&rest[..end]) {
            return value;
        }
    }
    panic!("non-JSON RPC response from {method}: {raw_body}");
}

pub fn set_account(rpc_url: &str, key: &Pubkey, owner: &Pubkey, lamports: u64, data: &[u8]) {
    let response = rpc_call(
        rpc_url,
        "surfnet_setAccount",
        serde_json::json!([
            key.to_string(),
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
        response.get("error").is_none(),
        "surfnet_setAccount failed for {key}: {response}",
    );
}
