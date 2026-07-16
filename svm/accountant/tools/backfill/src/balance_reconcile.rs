//! Reconcile committed accountant balances against the transfer history.
//!
//! Sums every `transfer` (and `modification`) row in a snapshot catalogue into
//! per-account credits/debits using the program's accounting sign rule, then
//! checks the net against the committed `account` rows in the same catalogue.
//! A clean run proves the committed transfer history nets to the committed
//! balances — i.e. the on-chain accounting logic, fed this real data, would
//! reproduce the wormchain-committed balances.
//!
//! Sign rule (mirrors `BalanceAccountLayout::lock_or_burn` / `unlock_or_mint`):
//!   - source side: credit if `emitter_chain == token_chain` (native lock),
//!     else debit (wrapped burn);
//!   - destination side: debit if `recipient_chain == token_chain` (native
//!     unlock), else credit (wrapped mint);
//!   - modification: credit on Add, debit on Subtract.
//!
//! We accumulate net credits/debits rather than applying the program's per-step
//! `checked_sub` directly, because the catalogue is sorted by
//! `(kind, chain, emitter, sequence)`, NOT globally chronological — a wrapped
//! burn can appear before the credit that funds it, which would trip an
//! intermediate (and spurious) underflow that never occurred on-chain. The net
//! is order-independent and is what must equal the committed balance.
//!
//! Both NTT and the WTT (Token Bridge) accountant share this catalogue schema
//! and balance arithmetic, so one tool covers both — point it at the respective
//! catalogue.

use std::collections::HashMap;

use anyhow::{Context, Result};

use crate::catalogue::{CatalogueError, ModifyKind, Record};
use global_accountant_definitions::Uint256;

/// Balance-account key: `(chain, token_chain, token_address)`.
type Key = (u16, u16, [u8; 32]);

/// Net accumulator for one account.
#[derive(Default, Clone, Copy)]
struct Net {
    credits: Uint256,
    debits: Uint256,
}

impl Net {
    fn credit(&mut self, amount: Uint256) -> Result<()> {
        self.credits = self.credits.checked_add(amount).context("credit overflow")?;
        Ok(())
    }
    fn debit(&mut self, amount: Uint256) -> Result<()> {
        self.debits = self.debits.checked_add(amount).context("debit overflow")?;
        Ok(())
    }
    /// Net balance `credits - debits`. For data consistent with a committed
    /// (non-negative) balance, credits >= debits.
    fn balance(&self) -> Option<Uint256> {
        self.credits.checked_sub(self.debits)
    }
}

/// Result of replaying a catalogue's transfer/modification history against
/// its committed balances.
#[derive(Debug, Default)]
pub struct ReconcileReport {
    pub transfers: u64,
    pub modifications: u64,
    pub accounts: u64,
    /// Committed balances whose replayed net matched exactly.
    pub matched: usize,
    /// One line per discrepancy: either a committed balance that didn't
    /// match its replayed net (`MISMATCH`), or replayed activity with no
    /// committed balance row and a non-zero net (`UNEXPECTED`).
    pub mismatches: Vec<String>,
}

impl ReconcileReport {
    /// No discrepancies — every committed balance reconciles.
    pub fn is_clean(&self) -> bool {
        self.mismatches.is_empty()
    }
}

/// Replay `records` and reconcile committed `account` balances against the
/// net of `transfer`/`modification` activity touching the same
/// `(chain, token_chain, token_address)` key.
pub fn reconcile(
    records: impl Iterator<Item = Result<Record, CatalogueError>>,
) -> Result<ReconcileReport> {
    let mut net: HashMap<Key, Net> = HashMap::new();
    let mut committed: HashMap<Key, Uint256> = HashMap::new();
    let mut report = ReconcileReport::default();

    for (i, record) in records.enumerate() {
        let record = record.with_context(|| format!("parse catalogue line {}", i + 1))?;
        match record {
            Record::Transfer(t) => {
                report.transfers += 1;
                let amount = Uint256::from_be_bytes(t.amount);
                let src = net.entry((t.chain, t.token_chain, t.token_address)).or_default();
                if t.chain == t.token_chain {
                    src.credit(amount)?; // native lock
                } else {
                    src.debit(amount)?; // wrapped burn
                }
                let dst = net
                    .entry((t.recipient_chain, t.token_chain, t.token_address))
                    .or_default();
                if t.recipient_chain == t.token_chain {
                    dst.debit(amount)?; // native unlock
                } else {
                    dst.credit(amount)?; // wrapped mint
                }
            }
            Record::Modification(m) => {
                report.modifications += 1;
                let amount = Uint256::from_be_bytes(m.amount);
                let acc = net.entry((m.chain_id, m.token_chain, m.token_address)).or_default();
                match m.modify_kind {
                    ModifyKind::Add => acc.credit(amount)?,
                    ModifyKind::Subtract => acc.debit(amount)?,
                }
            }
            Record::Account(a) => {
                report.accounts += 1;
                committed.insert(
                    (a.chain, a.token_chain, a.token_address),
                    Uint256::from_be_bytes(a.balance),
                );
            }
            // Registration / hub / peer rows carry no balance effect.
            _ => {}
        }
    }

    let fmt = |k: &Key| format!("chain={} token_chain={} token=0x{}", k.0, k.1, hex::encode(k.2));

    for (key, want) in &committed {
        match net.get(key).and_then(Net::balance) {
            Some(got) if got == *want => report.matched += 1,
            Some(got) => report
                .mismatches
                .push(format!("MISMATCH {} committed={want:?} net={got:?}", fmt(key))),
            None => report
                .mismatches
                .push(format!("MISMATCH {} committed={want:?} net=<negative>", fmt(key))),
        }
    }
    // Accounts with transfer activity but no committed balance row (should net to zero).
    let mut unexpected: Vec<Key> = net
        .iter()
        .filter(|(k, n)| !committed.contains_key(*k) && n.balance() != Some(Uint256::ZERO))
        .map(|(k, _)| *k)
        .collect();
    unexpected.sort();
    for k in &unexpected {
        let got = net.get(k).and_then(Net::balance);
        report
            .mismatches
            .push(format!("UNEXPECTED {} net={got:?} (no committed balance)", fmt(k)));
    }

    Ok(report)
}
