//! Streaming JSONL reader for the wormchain snapshot.
//!
//! The snapshot is the deterministic output of `tools/wormchain-snapshot/` —
//! one JSON object per line, sorted by `(kind, chain, emitter, sequence)`.
//! Row kinds: the Token-Bridge kinds (`transfer`, `account`,
//! `modification`, `registration`), `governance` (governance VAA digests),
//! and three NTT-native map kinds (`relayer_chain_registration`,
//! `transceiver_hub`, `transceiver_peer`).
//!
//! The reader is an iterator that parses one line at a time. Hex-encoded
//! 32-byte fields (emitter, digest, addresses, balances, amounts) are decoded
//! to `[u8; 32]` on parse; numeric fields are pulled as their native widths.

use std::fs::File;
use std::io::{BufRead, BufReader, Lines};
use std::path::Path;

use global_accountant_definitions::{GOVERNANCE_EMITTER, SOLANA_CHAIN_ID};
use sha2::{Digest, Sha256};

/// Modification direction, as the program stores it.
pub use global_accountant_definitions::ModificationKind;
use serde_json::Value;
use thiserror::Error;

/// One snapshot record. The `kind` discriminator in the source JSON maps to
/// the corresponding variant here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SnapshotRow {
    Transfer(TransferRow),
    Balance(BalanceRow),
    ModifyBalance(ModifyBalanceRow),
    TokenBridgeRegistration(ChainRegistrationRow),
    Governance(GovernanceRow),
    RelayerChainRegistration(ChainRegistrationRow),
    TransceiverHub(TransceiverHubRow),
    TransceiverPeer(TransceiverPeerRow),
}

impl SnapshotRow {
    /// The registration of either kind. `Accountant::accepts` decides which
    /// kind a snapshot may hold.
    pub fn chain_registration(&self) -> Option<&ChainRegistrationRow> {
        match self {
            SnapshotRow::TokenBridgeRegistration(r) | SnapshotRow::RelayerChainRegistration(r) => {
                Some(r)
            }
            _ => None,
        }
    }
}

/// `kind: "transfer"` — one VAA-committed token-bridge transfer captured by
/// wormchain's accountant. Fed into the on-chain `BackfillNoReplay` ix as
/// `(chain, emitter, sequence, digest)`; the remaining payload fields are
/// preserved for cross-checks and future audit-tool use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransferRow {
    pub chain: u16,
    pub emitter: [u8; 32],
    pub sequence: u64,
    pub digest: [u8; 32],
    pub amount: [u8; 32],
    pub token_chain: u16,
    pub token_address: [u8; 32],
    pub recipient_chain: u16,
}

/// `kind: "account"` — one `(chain, token_chain, token_address)` Balance
/// entry. Fed into `BackfillBalance` verbatim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BalanceRow {
    pub chain: u16,
    pub token_chain: u16,
    pub token_address: [u8; 32],
    pub balance: [u8; 32],
}

/// `kind: "modification"` — a governance Modify-Balance entry. Fed into
/// `BackfillModifyBalance`, which writes the record PDA only: the snapshot
/// balances already include every modification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModifyBalanceRow {
    /// Modification sequence from the governance payload, not the VAA sequence.
    pub sequence: u64,
    pub chain_id: u16,
    pub token_chain: u16,
    pub token_address: [u8; 32],
    pub amount: [u8; 32],
    /// Governance wire form: ASCII, zero-left-padded to 32 bytes.
    pub reason: [u8; 32],
    pub modify_kind: ModificationKind,
}

/// `kind: "registration"` (Token Bridge) or `kind:
/// "relayer_chain_registration"` (Standard Relayer, NTT accountant). Fed into
/// `BackfillChainRegistration` (WTT) / `BackfillRelayerChainRegistration`
/// (NTT), which write the `ChainRegistration` PDA and the `RegisterChain`
/// record PDA.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChainRegistrationRow {
    pub chain: u16,
    pub registered_emitter: [u8; 32],
    /// Sequence of the governance VAA that registered the emitter. Sourced
    /// from the VAA archive; the wormchain registration map holds the emitter only.
    pub sequence: u64,
}

/// SHA-256 of a file, streamed. The progress file identity and the parity ledger
/// both record this hash of the snapshot.
pub fn file_sha256(path: &Path) -> std::io::Result<[u8; 32]> {
    let mut hasher = Sha256::new();
    std::io::copy(&mut File::open(path)?, &mut hasher)?;
    Ok(hasher.finalize().into())
}

/// `kind: "governance"` — one wormchain `DIGESTS` row under
/// `(SOLANA_CHAIN_ID, GOVERNANCE_EMITTER)`. Not backfilled: a governance
/// sequence is random, and a marked bit would pre-burn it for `upgrade_contract`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GovernanceRow {
    pub chain: u16,
    pub emitter: [u8; 32],
    pub sequence: u64,
    pub digest: [u8; 32],
}

/// `kind: "transceiver_hub"` — one transceiver→hub mapping. Keyed by
/// `(chain, address)`, storing the `(hub_chain, hub_address)` the transceiver
/// belongs to. Fed into `BackfillTransceiverHub`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransceiverHubRow {
    pub chain: u16,
    pub address: [u8; 32],
    pub hub_chain: u16,
    pub hub_address: [u8; 32],
}

/// `kind: "transceiver_peer"` — one transceiver→peer mapping. Keyed by
/// `(chain, address, dest_chain)`, storing the registered `peer_address` on
/// `dest_chain`. Fed into `BackfillTransceiverPeer`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransceiverPeerRow {
    pub chain: u16,
    pub address: [u8; 32],
    pub dest_chain: u16,
    pub peer_address: [u8; 32],
}

#[derive(Debug, Error)]
pub enum SnapshotError {
    #[error("I/O error reading snapshot: {0}")]
    Io(#[from] std::io::Error),
    #[error("line {line}: invalid JSON: {source}")]
    Json {
        line: usize,
        #[source]
        source: serde_json::Error,
    },
    #[error("line {line}: record missing required field `{field}`")]
    MissingField { line: usize, field: &'static str },
    #[error("line {line}: unknown record kind `{kind}`")]
    UnknownKind { line: usize, kind: String },
    #[error("line {line}: field `{field}` is not a {expected}")]
    BadType {
        line: usize,
        field: &'static str,
        expected: &'static str,
    },
    #[error("line {line}: field `{field}` invalid hex: {msg}")]
    InvalidHex {
        line: usize,
        field: &'static str,
        msg: String,
    },
    #[error("line {line}: field `{field}` value {value} out of range for {expected}")]
    OutOfRange {
        line: usize,
        field: &'static str,
        value: u64,
        expected: &'static str,
    },
    #[error("line {line}: modify_kind must be \"add\" or \"subtract\", got `{value}`")]
    BadModifyKind { line: usize, value: String },
    #[error(
        "line {line}: reason must be at most 32 ASCII bytes, got {len} byte(s), ascii={ascii}"
    )]
    BadReason {
        line: usize,
        len: usize,
        ascii: bool,
    },
    #[error("line {line}: `{kind}` row under (chain {chain}, emitter {emitter}); want `{want}`")]
    WrongEmitterKind {
        line: usize,
        kind: &'static str,
        chain: u16,
        emitter: String,
        want: &'static str,
    },
}

/// Streaming iterator over the snapshot. Constructed via [`SnapshotReader::open`].
///
/// `Iterator::Item = Result<SnapshotRow, SnapshotError>` so callers can decide
/// whether a single malformed line should abort the run (via
/// `collect::<Result<_, _>>()`) or be filtered + logged.
pub struct SnapshotReader {
    lines: Lines<BufReader<File>>,
    line_number: usize,
}

impl SnapshotReader {
    /// Open the snapshot at `path`. Lazy — no records are parsed until
    /// `next()` is called.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, SnapshotError> {
        let file = File::open(path)?;
        let reader = BufReader::new(file);
        Ok(Self {
            lines: reader.lines(),
            line_number: 0,
        })
    }
}

impl Iterator for SnapshotReader {
    type Item = Result<SnapshotRow, SnapshotError>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            self.line_number += 1;
            let raw = self.lines.next()?;
            let line = match raw {
                Ok(s) => s,
                Err(e) => return Some(Err(SnapshotError::Io(e))),
            };
            if line.trim().is_empty() {
                continue;
            }
            return Some(parse_record(self.line_number, &line));
        }
    }
}

// ============================================================================
// Parsing
// ============================================================================

fn parse_record(line: usize, raw: &str) -> Result<SnapshotRow, SnapshotError> {
    let v: Value =
        serde_json::from_str(raw).map_err(|source| SnapshotError::Json { line, source })?;
    let kind = v
        .get("kind")
        .and_then(Value::as_str)
        .ok_or(SnapshotError::MissingField {
            line,
            field: "kind",
        })?;
    match kind {
        "transfer" => Ok(SnapshotRow::Transfer(parse_transfer(line, &v)?)),
        "account" => Ok(SnapshotRow::Balance(parse_account(line, &v)?)),
        "modification" => Ok(SnapshotRow::ModifyBalance(parse_modification(line, &v)?)),
        "registration" => Ok(SnapshotRow::TokenBridgeRegistration(parse_registration(
            line, &v,
        )?)),
        "governance" => Ok(SnapshotRow::Governance(parse_governance(line, &v)?)),
        "relayer_chain_registration" => Ok(SnapshotRow::RelayerChainRegistration(
            parse_registration(line, &v)?,
        )),
        "transceiver_hub" => Ok(SnapshotRow::TransceiverHub(parse_transceiver_hub(
            line, &v,
        )?)),
        "transceiver_peer" => Ok(SnapshotRow::TransceiverPeer(parse_transceiver_peer(
            line, &v,
        )?)),
        other => Err(SnapshotError::UnknownKind {
            line,
            kind: other.to_owned(),
        }),
    }
}

/// `(SOLANA_CHAIN_ID, GOVERNANCE_EMITTER)`: the wormchain `DIGESTS` key of a governance VAA.
pub fn is_governance_emitter(chain: u16, emitter: &[u8; 32]) -> bool {
    chain == SOLANA_CHAIN_ID && *emitter == GOVERNANCE_EMITTER
}

fn parse_transfer(line: usize, v: &Value) -> Result<TransferRow, SnapshotError> {
    let row = TransferRow {
        chain: get_u16(line, v, "chain")?,
        emitter: get_hex32(line, v, "emitter")?,
        sequence: get_u64(line, v, "sequence")?,
        digest: get_hex32(line, v, "digest")?,
        amount: get_hex32(line, v, "amount")?,
        token_chain: get_u16(line, v, "token_chain")?,
        token_address: get_hex32(line, v, "token_address")?,
        recipient_chain: get_u16(line, v, "recipient_chain")?,
    };
    // SECURITY: a governance digest backfilled as a transfer would pre-burn its
    // NoReplay bit, which `upgrade_contract` consults.
    if is_governance_emitter(row.chain, &row.emitter) {
        return Err(SnapshotError::WrongEmitterKind {
            line,
            kind: "transfer",
            chain: row.chain,
            emitter: hex::encode(row.emitter),
            want: "governance",
        });
    }
    Ok(row)
}

fn parse_governance(line: usize, v: &Value) -> Result<GovernanceRow, SnapshotError> {
    let row = GovernanceRow {
        chain: get_u16(line, v, "chain")?,
        emitter: get_hex32(line, v, "emitter")?,
        sequence: get_u64(line, v, "sequence")?,
        digest: get_hex32(line, v, "digest")?,
    };
    if !is_governance_emitter(row.chain, &row.emitter) {
        return Err(SnapshotError::WrongEmitterKind {
            line,
            kind: "governance",
            chain: row.chain,
            emitter: hex::encode(row.emitter),
            want: "transfer",
        });
    }
    Ok(row)
}

fn parse_account(line: usize, v: &Value) -> Result<BalanceRow, SnapshotError> {
    Ok(BalanceRow {
        chain: get_u16(line, v, "chain")?,
        token_chain: get_u16(line, v, "token_chain")?,
        token_address: get_hex32(line, v, "token_address")?,
        balance: get_hex32(line, v, "balance")?,
    })
}

fn parse_modification(line: usize, v: &Value) -> Result<ModifyBalanceRow, SnapshotError> {
    let modify_kind = match get_str(line, v, "modify_kind")? {
        "add" => ModificationKind::Add,
        "subtract" => ModificationKind::Subtract,
        other => {
            return Err(SnapshotError::BadModifyKind {
                line,
                value: other.to_owned(),
            })
        }
    };
    Ok(ModifyBalanceRow {
        sequence: get_u64(line, v, "sequence")?,
        chain_id: get_u16(line, v, "chain_id")?,
        token_chain: get_u16(line, v, "token_chain")?,
        token_address: get_hex32(line, v, "token_address")?,
        amount: get_hex32(line, v, "amount")?,
        reason: encode_reason(line, get_str(line, v, "reason")?)?,
        modify_kind,
    })
}

/// Governance wire form of a reason: zero-left-padded to 32 bytes
/// (`sdk/rust/vaas-serde/src/arraystring.rs`). The program stores these bytes as-is.
pub fn encode_reason(line: usize, reason: &str) -> Result<[u8; 32], SnapshotError> {
    let bytes = reason.as_bytes();
    if bytes.len() > 32 || !bytes.is_ascii() {
        return Err(SnapshotError::BadReason {
            line,
            len: bytes.len(),
            ascii: bytes.is_ascii(),
        });
    }
    let mut out = [0u8; 32];
    out[32 - bytes.len()..].copy_from_slice(bytes);
    Ok(out)
}

fn parse_registration(line: usize, v: &Value) -> Result<ChainRegistrationRow, SnapshotError> {
    Ok(ChainRegistrationRow {
        chain: get_u16(line, v, "chain")?,
        registered_emitter: get_hex32(line, v, "registered_emitter")?,
        sequence: get_u64(line, v, "sequence")?,
    })
}

fn parse_transceiver_hub(line: usize, v: &Value) -> Result<TransceiverHubRow, SnapshotError> {
    Ok(TransceiverHubRow {
        chain: get_u16(line, v, "chain")?,
        address: get_hex32(line, v, "address")?,
        hub_chain: get_u16(line, v, "hub_chain")?,
        hub_address: get_hex32(line, v, "hub_address")?,
    })
}

fn parse_transceiver_peer(line: usize, v: &Value) -> Result<TransceiverPeerRow, SnapshotError> {
    Ok(TransceiverPeerRow {
        chain: get_u16(line, v, "chain")?,
        address: get_hex32(line, v, "address")?,
        dest_chain: get_u16(line, v, "dest_chain")?,
        peer_address: get_hex32(line, v, "peer_address")?,
    })
}

// ----- field accessors -----

fn get_str<'a>(line: usize, v: &'a Value, field: &'static str) -> Result<&'a str, SnapshotError> {
    v.get(field)
        .ok_or(SnapshotError::MissingField { line, field })?
        .as_str()
        .ok_or(SnapshotError::BadType {
            line,
            field,
            expected: "string",
        })
}

fn get_u64(line: usize, v: &Value, field: &'static str) -> Result<u64, SnapshotError> {
    v.get(field)
        .ok_or(SnapshotError::MissingField { line, field })?
        .as_u64()
        .ok_or(SnapshotError::BadType {
            line,
            field,
            expected: "u64",
        })
}

fn get_u16(line: usize, v: &Value, field: &'static str) -> Result<u16, SnapshotError> {
    let n = get_u64(line, v, field)?;
    u16::try_from(n).map_err(|_| SnapshotError::OutOfRange {
        line,
        field,
        value: n,
        expected: "u16",
    })
}

fn get_hex32(line: usize, v: &Value, field: &'static str) -> Result<[u8; 32], SnapshotError> {
    let s = get_str(line, v, field)?;
    let stripped = s.strip_prefix("0x").unwrap_or(s);
    if stripped.len() != 64 {
        return Err(SnapshotError::InvalidHex {
            line,
            field,
            msg: format!("expected 64 hex chars (32 bytes), got {}", stripped.len()),
        });
    }
    let bytes = hex::decode(stripped).map_err(|e| SnapshotError::InvalidHex {
        line,
        field,
        msg: e.to_string(),
    })?;
    let mut out = [0u8; 32];
    out.copy_from_slice(&bytes);
    Ok(out)
}
