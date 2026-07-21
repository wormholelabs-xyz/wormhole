//! Minimal hand-rolled JSON-RPC HTTP mock server for `submitter::submit_chunks`
//! orchestration tests.
//!
//! `submit_chunks` always constructs its own `RpcClient::new_with_commitment`
//! internally, which always wires up the real `HttpSender` — there is no
//! injection point for a fake `RpcSender`. Exercising the orchestration loop
//! (retry/backoff, halt behavior, concurrent stats aggregation) therefore
//! needs a real HTTP endpoint. `HttpSender::send` only requires a POST to a
//! single fixed URL returning HTTP 200 with a `"result"`/`"error"` JSON body
//! — little enough to hand-roll rather than pull in a server framework.

use std::collections::VecDeque;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use base64::{engine::general_purpose::STANDARD, Engine};
use serde_json::{json, Value};
use solana_hash::Hash;
use solana_signature::Signature;

/// Scripted behavior for the Nth `sendTransaction` call the server receives,
/// consumed FIFO across ALL connections (the submitter may have several
/// chunks in flight concurrently, so there is no per-chunk correlation —
/// tests that care which chunk gets which behavior should use
/// `concurrency: 1`).
#[derive(Clone, Debug)]
pub enum SendTxBehavior {
    /// Succeed: echoes back the transaction's own embedded signature (as
    /// `send_transaction_with_config` requires — it rejects a mismatched
    /// signature), so `send_and_confirm_transaction`'s later
    /// `getSignatureStatuses` poll can resolve it as confirmed.
    Success,
    /// A JSON-RPC error whose message embeds `custom program error: 0x{code:x}`
    /// — drives `classify()`'s `AlreadyAccounted` (code 7) / `Halt` (any other
    /// code) branches exactly as a real preflight failure would.
    CustomProgramError(u32),
    /// A generic RPC-shaped error with no custom-program-error substring —
    /// drives `classify()`'s `Retry` branch.
    TransientError,
}

#[derive(Default)]
struct Inner {
    script: Mutex<VecDeque<SendTxBehavior>>,
    send_transaction_calls: AtomicUsize,
    /// Pre-built `RpcKeyedAccount`-shaped JSON values served verbatim as the
    /// `getProgramAccounts` result — used by the NTT decode-closure tests in
    /// `tests/reconcile.rs`, which need `fetch_tagged_accounts`'s real gPA
    /// round trip (not just a hand-copied byte-offset reimplementation).
    program_accounts: Vec<Value>,
}

pub struct MockRpc {
    inner: Arc<Inner>,
    url: String,
}

impl MockRpc {
    /// Start the server on an ephemeral localhost port, with `script`
    /// consumed FIFO by successive `sendTransaction` calls. Once the script
    /// is exhausted, further calls default to `Success` (so tests only need
    /// to script the calls they care about).
    pub fn start(script: Vec<SendTxBehavior>) -> Self {
        Self::start_inner(script, Vec::new())
    }

    /// Start a server whose `getProgramAccounts` responses are exactly
    /// `accounts` (build entries with [`keyed_account_json`]), for any
    /// filters — the mock does not implement `Memcmp`/`DataSize` filtering,
    /// it just returns the whole scripted set, matching how narrowly these
    /// tests are scoped (one account class at a time).
    pub fn start_with_program_accounts(accounts: Vec<Value>) -> Self {
        Self::start_inner(Vec::new(), accounts)
    }

    fn start_inner(script: Vec<SendTxBehavior>, program_accounts: Vec<Value>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
        let addr = listener.local_addr().expect("local_addr");
        let inner = Arc::new(Inner {
            script: Mutex::new(script.into()),
            send_transaction_calls: AtomicUsize::new(0),
            program_accounts,
        });
        let accept_inner = inner.clone();
        thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { continue };
                let inner = accept_inner.clone();
                thread::spawn(move || handle_connection(stream, &inner));
            }
        });
        Self {
            inner,
            url: format!("http://{addr}"),
        }
    }

    pub fn url(&self) -> String {
        self.url.clone()
    }

    /// Total `sendTransaction` calls observed so far — lets a test prove
    /// whether the submitter actually stopped dispatching new work after a
    /// halt-class error, or drained the whole input regardless.
    pub fn send_transaction_calls(&self) -> usize {
        self.inner.send_transaction_calls.load(Ordering::SeqCst)
    }
}

/// Build one `RpcKeyedAccount`-shaped JSON value: `{pubkey, account: {lamports,
/// data: [base64, "base64"], owner, executable, rentEpoch}}`. `owner` need not
/// be the real backfill program id — `UiAccount::decode()` just parses it as
/// a `Pubkey`, it doesn't cross-check the request's target program.
pub fn keyed_account_json(pubkey: &solana_pubkey::Pubkey, owner: &solana_pubkey::Pubkey, data: &[u8]) -> Value {
    json!({
        "pubkey": pubkey.to_string(),
        "account": {
            "lamports": 1_000_000,
            "data": [STANDARD.encode(data), "base64"],
            "owner": owner.to_string(),
            "executable": false,
            "rentEpoch": 0,
            "space": data.len(),
        },
    })
}

fn handle_connection(mut stream: TcpStream, inner: &Inner) {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    let header_end = loop {
        let n = match stream.read(&mut chunk) {
            Ok(0) | Err(_) => return,
            Ok(n) => n,
        };
        buf.extend_from_slice(&chunk[..n]);
        if let Some(pos) = find_subslice(&buf, b"\r\n\r\n") {
            break pos + 4;
        }
        if buf.len() > 4_000_000 {
            return; // safety valve against a malformed/runaway request
        }
    };

    let header_str = String::from_utf8_lossy(&buf[..header_end]).into_owned();
    let content_length: usize = header_str
        .lines()
        .find_map(|line| {
            let lower = line.to_ascii_lowercase();
            lower
                .strip_prefix("content-length:")
                .map(|v| v.trim().to_string())
        })
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);

    while buf.len() < header_end + content_length {
        let n = match stream.read(&mut chunk) {
            Ok(0) | Err(_) => return,
            Ok(n) => n,
        };
        buf.extend_from_slice(&chunk[..n]);
    }
    let body = &buf[header_end..header_end + content_length];
    let request: Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => return,
    };
    let method = request.get("method").and_then(Value::as_str).unwrap_or("");
    let params = request.get("params").cloned().unwrap_or(Value::Null);

    let response = dispatch(method, &params, inner);
    let body = response.to_string();
    let http = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    );
    let _ = stream.write_all(http.as_bytes());
}

fn dispatch(method: &str, params: &Value, inner: &Inner) -> Value {
    match method {
        "getLatestBlockhash" => json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": {
                "context": {"slot": 1},
                "value": {
                    "blockhash": Hash::new_from_array([7u8; 32]).to_string(),
                    "lastValidBlockHeight": 1_000_000,
                },
            },
        }),
        "isBlockhashValid" => json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": {"context": {"slot": 1}, "value": true},
        }),
        "sendTransaction" => {
            inner.send_transaction_calls.fetch_add(1, Ordering::SeqCst);
            let behavior = inner
                .script
                .lock()
                .expect("script lock")
                .pop_front()
                .unwrap_or(SendTxBehavior::Success);
            send_transaction_response(params, behavior)
        }
        "getProgramAccounts" => json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": inner.program_accounts.clone(),
        }),
        "getSignatureStatuses" => {
            let sigs = params
                .get(0)
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let statuses: Vec<Value> = sigs
                .iter()
                .map(|_| {
                    json!({
                        "slot": 2,
                        "confirmations": null,
                        "status": {"Ok": null},
                        "err": null,
                        "confirmationStatus": "finalized",
                    })
                })
                .collect();
            json!({
                "jsonrpc": "2.0",
                "id": 1,
                "result": {"context": {"slot": 2}, "value": statuses},
            })
        }
        _ => json!({"jsonrpc": "2.0", "id": 1, "result": Value::Null}),
    }
}

fn send_transaction_response(params: &Value, behavior: SendTxBehavior) -> Value {
    match behavior {
        SendTxBehavior::Success => {
            let encoded = params
                .get(0)
                .and_then(Value::as_str)
                .expect("sendTransaction params[0] is the base64 tx");
            let raw = STANDARD.decode(encoded).expect("valid base64 tx");
            // Wire format: `[compact-u16 sig count][sig 0 (64B)]...[message]`.
            // Every tx `submit_ix_with_retries` builds has exactly one
            // signer, so the compact-u16 count fits in a single byte (1).
            assert_eq!(raw[0], 1, "mock only supports single-signer transactions");
            let sig_bytes: [u8; 64] = raw[1..65].try_into().expect("64-byte signature");
            let signature = Signature::from(sig_bytes);
            json!({"jsonrpc": "2.0", "id": 1, "result": signature.to_string()})
        }
        SendTxBehavior::CustomProgramError(code) => json!({
            "jsonrpc": "2.0",
            "id": 1,
            "error": {
                "code": -32002,
                "message": format!(
                    "Transaction simulation failed: Error processing Instruction 0: custom program error: 0x{code:x}"
                ),
            },
        }),
        SendTxBehavior::TransientError => json!({
            "jsonrpc": "2.0",
            "id": 1,
            "error": {
                "code": -32005,
                "message": "Node is behind by 42 slots",
            },
        }),
    }
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}
