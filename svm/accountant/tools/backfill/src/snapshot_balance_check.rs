//! Check the snapshot's committed balances against its own transfer history.
//!
//! Sums every `transfer` (and `modification`) row in a snapshot into
//! per-account credits/debits using the program's accounting sign rule, then
//! checks the net against the committed `account` rows in the same snapshot.
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
//! `checked_sub` directly, because the snapshot is sorted by
//! `(kind, chain, emitter, sequence)`, NOT globally chronological — a wrapped
//! burn can appear before the credit that funds it, which would trip an
//! intermediate (and spurious) underflow that never occurred on-chain. The net
//! is order-independent and is what must equal the committed balance.
//!
//! Both NTT and the WTT (Token Bridge) accountant share this snapshot schema
//! and balance arithmetic, so one tool covers both — point it at the respective
//! snapshot.

use std::collections::HashMap;

use anyhow::{Context, Result};

use global_accountant_definitions::Uint256;
use svm_accountant_tools_common::ids::BalanceKey;
use svm_accountant_tools_common::snapshot::{ModificationKind, SnapshotError, SnapshotRow};

/// Net accumulator for one account.
#[derive(Default, Clone, Copy)]
struct Net {
    credits: Uint256,
    debits: Uint256,
}

impl Net {
    fn credit(&mut self, amount: Uint256) -> Result<()> {
        self.credits = self
            .credits
            .checked_add(amount)
            .context("credit overflow")?;
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

/// Result of netting a snapshot's transfer/modification history against
/// its committed balances.
#[derive(Debug, Default)]
pub struct SnapshotBalanceReport {
    pub transfers: u64,
    pub modifications: u64,
    pub balances: u64,
    /// Committed balances whose replayed net matched exactly.
    pub matched: usize,
    /// One line per discrepancy: either a committed balance that didn't
    /// match its replayed net (`MISMATCH`), or replayed activity with no
    /// committed balance row and a non-zero net (`UNEXPECTED`).
    pub discrepancies: Vec<String>,
}

impl SnapshotBalanceReport {
    /// No discrepancies: every committed balance equals its net.
    pub fn is_clean(&self) -> bool {
        self.discrepancies.is_empty()
    }
}

/// Net `rows` and check each committed balance against the net of the
/// `transfer`/`modification` activity on the same
/// `(chain, token_chain, token_address)` key.
pub fn check_snapshot_balances(
    rows: impl Iterator<Item = Result<SnapshotRow, SnapshotError>>,
) -> Result<SnapshotBalanceReport> {
    let mut net: HashMap<BalanceKey, Net> = HashMap::new();
    let mut committed: HashMap<BalanceKey, Uint256> = HashMap::new();
    let mut report = SnapshotBalanceReport::default();

    for row in rows {
        let row = row.context("parse snapshot")?;
        match row {
            SnapshotRow::Transfer(t) => {
                report.transfers += 1;
                let amount = t.amount;
                let src = net
                    .entry(BalanceKey {
                        chain: t.chain,
                        token_chain: t.token_chain,
                        token_address: t.token_address,
                    })
                    .or_default();
                if t.chain == t.token_chain {
                    src.credit(amount)?; // native lock
                } else {
                    src.debit(amount)?; // wrapped burn
                }
                let dst = net
                    .entry(BalanceKey {
                        chain: t.recipient_chain,
                        token_chain: t.token_chain,
                        token_address: t.token_address,
                    })
                    .or_default();
                if t.recipient_chain == t.token_chain {
                    dst.debit(amount)?; // native unlock
                } else {
                    dst.credit(amount)?; // wrapped mint
                }
            }
            SnapshotRow::ModifyBalance(m) => {
                report.modifications += 1;
                let amount = m.amount;
                let acc = net
                    .entry(BalanceKey {
                        chain: m.chain_id,
                        token_chain: m.token_chain,
                        token_address: m.token_address,
                    })
                    .or_default();
                match m.modify_kind {
                    ModificationKind::Add => acc.credit(amount)?,
                    ModificationKind::Subtract => acc.debit(amount)?,
                }
            }
            SnapshotRow::Balance(a) => {
                report.balances += 1;
                if committed.insert(a.key(), a.balance).is_some() {
                    anyhow::bail!("two committed balance rows for {}", a.key());
                }
            }
            // Registration / hub / peer rows carry no balance effect.
            _ => {}
        }
    }

    for (key, want) in &committed {
        match net.get(key).and_then(Net::balance) {
            Some(got) if got == *want => report.matched += 1,
            Some(got) => report
                .discrepancies
                .push(format!("MISMATCH {key} committed={want:?} net={got:?}")),
            None => report
                .discrepancies
                .push(format!("MISMATCH {key} committed={want:?} net=<negative>")),
        }
    }
    // Accounts with transfer activity but no committed balance row (should net to zero).
    let mut unexpected: Vec<BalanceKey> = net
        .iter()
        .filter(|(k, n)| !committed.contains_key(*k) && n.balance() != Some(Uint256::ZERO))
        .map(|(k, _)| *k)
        .collect();
    unexpected.sort();
    for k in &unexpected {
        let got = net.get(k).and_then(Net::balance);
        report
            .discrepancies
            .push(format!("UNEXPECTED {k} net={got:?} (no committed balance)"));
    }

    Ok(report)
}
