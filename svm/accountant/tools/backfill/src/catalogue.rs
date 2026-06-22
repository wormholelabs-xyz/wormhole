//! Streaming JSONL reader for the wormchain snapshot catalogue.
//!
//! The catalogue is the deterministic output of `tools/wormchain-snapshot/` —
//! one JSON object per line, sorted by `(kind, chain, emitter, sequence)`.
//! Four record kinds: `transfer`, `account`, `modification`, `registration`.
//! See `accountant-migration-backfill.md` §"Contract between A and B" for the
//! schema.
//!
//! The reader is iterator-based and bounded-memory — it parses one record at a
//! time, never loading the whole 2.1 GB catalogue into RAM. Hex-encoded
//! 32-byte fields (emitter, digest, addresses, balances, amounts) are decoded
//! to `[u8; 32]` on parse; numeric fields are pulled as their native widths.

use std::fs::File;
use std::io::{BufRead, BufReader, Lines};
use std::path::Path;

use serde_json::Value;
use thiserror::Error;

/// One catalogue record. The `kind` discriminator in the source JSON maps to
/// the corresponding variant here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Record {
    Transfer(TransferRecord),
    Account(AccountRecord),
    Modification(ModificationRecord),
    Registration(RegistrationRecord),
}

/// `kind: "transfer"` — one VAA-committed token-bridge transfer captured by
/// wormchain's accountant. Fed into the on-chain `BackfillNoReplay` ix as
/// `(chain, emitter, sequence, digest)`; the remaining payload fields are
/// preserved for cross-checks and future audit-tool use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransferRecord {
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
pub struct AccountRecord {
    pub chain: u16,
    pub token_chain: u16,
    pub token_address: [u8; 32],
    pub balance: [u8; 32],
}

/// `kind: "modification"` — a governance Modify-Balance entry. Replayed via
/// the operational program's `ModifyBalance` ix post-upgrade (Phase 7).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModificationRecord {
    pub sequence: u64,
    pub chain_id: u16,
    pub token_chain: u16,
    pub token_address: [u8; 32],
    pub amount: [u8; 32],
    pub reason: String,
    pub modify_kind: ModifyKind,
}

/// `kind: "registration"` — a token-bridge `RegisterChain` governance entry.
/// Replayed via the operational program's `RegisterChain` ix post-upgrade.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegistrationRecord {
    pub chain: u16,
    pub registered_emitter: [u8; 32],
}

/// Modification direction. Maps to the on-chain `ModificationKind` enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModifyKind {
    Add,
    Subtract,
}

#[derive(Debug, Error)]
pub enum CatalogueError {
    #[error("I/O error reading catalogue: {0}")]
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
}

/// Streaming iterator over the catalogue. Constructed via [`CatalogueReader::open`].
///
/// `Iterator::Item = Result<Record, CatalogueError>` so callers can decide
/// whether a single malformed line should abort the run (via
/// `collect::<Result<_, _>>()`) or be filtered + logged.
pub struct CatalogueReader {
    lines: Lines<BufReader<File>>,
    line_number: usize,
}

impl CatalogueReader {
    /// Open the catalogue at `path`. Lazy — no records are parsed until
    /// `next()` is called.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, CatalogueError> {
        let file = File::open(path)?;
        let reader = BufReader::new(file);
        Ok(Self {
            lines: reader.lines(),
            line_number: 0,
        })
    }
}

impl Iterator for CatalogueReader {
    type Item = Result<Record, CatalogueError>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            self.line_number += 1;
            let raw = self.lines.next()?;
            let line = match raw {
                Ok(s) => s,
                Err(e) => return Some(Err(CatalogueError::Io(e))),
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

fn parse_record(line: usize, raw: &str) -> Result<Record, CatalogueError> {
    let v: Value =
        serde_json::from_str(raw).map_err(|source| CatalogueError::Json { line, source })?;
    let kind = v
        .get("kind")
        .and_then(Value::as_str)
        .ok_or(CatalogueError::MissingField {
            line,
            field: "kind",
        })?;
    match kind {
        "transfer" => Ok(Record::Transfer(parse_transfer(line, &v)?)),
        "account" => Ok(Record::Account(parse_account(line, &v)?)),
        "modification" => Ok(Record::Modification(parse_modification(line, &v)?)),
        "registration" => Ok(Record::Registration(parse_registration(line, &v)?)),
        other => Err(CatalogueError::UnknownKind {
            line,
            kind: other.to_owned(),
        }),
    }
}

fn parse_transfer(line: usize, v: &Value) -> Result<TransferRecord, CatalogueError> {
    Ok(TransferRecord {
        chain: get_u16(line, v, "chain")?,
        emitter: get_hex32(line, v, "emitter")?,
        sequence: get_u64(line, v, "sequence")?,
        digest: get_hex32(line, v, "digest")?,
        amount: get_hex32(line, v, "amount")?,
        token_chain: get_u16(line, v, "token_chain")?,
        token_address: get_hex32(line, v, "token_address")?,
        recipient_chain: get_u16(line, v, "recipient_chain")?,
    })
}

fn parse_account(line: usize, v: &Value) -> Result<AccountRecord, CatalogueError> {
    Ok(AccountRecord {
        chain: get_u16(line, v, "chain")?,
        token_chain: get_u16(line, v, "token_chain")?,
        token_address: get_hex32(line, v, "token_address")?,
        balance: get_hex32(line, v, "balance")?,
    })
}

fn parse_modification(line: usize, v: &Value) -> Result<ModificationRecord, CatalogueError> {
    let modify_kind = match get_str(line, v, "modify_kind")? {
        "add" => ModifyKind::Add,
        "subtract" => ModifyKind::Subtract,
        other => {
            return Err(CatalogueError::BadModifyKind {
                line,
                value: other.to_owned(),
            })
        }
    };
    Ok(ModificationRecord {
        sequence: get_u64(line, v, "sequence")?,
        chain_id: get_u16(line, v, "chain_id")?,
        token_chain: get_u16(line, v, "token_chain")?,
        token_address: get_hex32(line, v, "token_address")?,
        amount: get_hex32(line, v, "amount")?,
        reason: get_str(line, v, "reason")?.to_owned(),
        modify_kind,
    })
}

fn parse_registration(line: usize, v: &Value) -> Result<RegistrationRecord, CatalogueError> {
    Ok(RegistrationRecord {
        chain: get_u16(line, v, "chain")?,
        registered_emitter: get_hex32(line, v, "registered_emitter")?,
    })
}

// ----- field accessors -----

fn get_str<'a>(
    line: usize,
    v: &'a Value,
    field: &'static str,
) -> Result<&'a str, CatalogueError> {
    v.get(field)
        .ok_or(CatalogueError::MissingField { line, field })?
        .as_str()
        .ok_or(CatalogueError::BadType {
            line,
            field,
            expected: "string",
        })
}

fn get_u64(line: usize, v: &Value, field: &'static str) -> Result<u64, CatalogueError> {
    v.get(field)
        .ok_or(CatalogueError::MissingField { line, field })?
        .as_u64()
        .ok_or(CatalogueError::BadType {
            line,
            field,
            expected: "u64",
        })
}

fn get_u16(line: usize, v: &Value, field: &'static str) -> Result<u16, CatalogueError> {
    let n = get_u64(line, v, field)?;
    u16::try_from(n).map_err(|_| CatalogueError::OutOfRange {
        line,
        field,
        value: n,
        expected: "u16",
    })
}

fn get_hex32(line: usize, v: &Value, field: &'static str) -> Result<[u8; 32], CatalogueError> {
    let s = get_str(line, v, field)?;
    let stripped = s.strip_prefix("0x").unwrap_or(s);
    if stripped.len() != 64 {
        return Err(CatalogueError::InvalidHex {
            line,
            field,
            msg: format!("expected 64 hex chars (32 bytes), got {}", stripped.len()),
        });
    }
    let bytes = hex::decode(stripped).map_err(|e| CatalogueError::InvalidHex {
        line,
        field,
        msg: e.to_string(),
    })?;
    let mut out = [0u8; 32];
    out.copy_from_slice(&bytes);
    Ok(out)
}
