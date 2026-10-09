//! Wormchain side of the comparison: the expected `ChainState`, built from the snapshot.
//! Expected account bytes come from `ga_backfill::expected_accounts`, the
//! constructors, the encoding the backfill writes.

use std::path::Path;

use anyhow::{bail, Context, Result};
use ga_backfill::accountant::Accountant;
use ga_backfill::expected_accounts::{
    balance_account, chain_registration_account, modify_balance_account, register_chain_account,
    transceiver_hub_account, transceiver_peer_account,
};
use ga_backfill::snapshot::{SnapshotReader, SnapshotRow};

use crate::state::ChainState;

/// Snapshot rows not compared on Solana.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct SkippedRows {
    /// `governance` rows: wormchain digests of governance VAAs, excluded from the backfill.
    pub governance_digests: usize,
}

pub fn expected_state_from_snapshot(
    path: &Path,
    accountant: Accountant,
) -> Result<(ChainState, SkippedRows)> {
    let mut state = ChainState::default();
    let mut skipped = SkippedRows::default();
    let reader =
        SnapshotReader::open(path).with_context(|| format!("open snapshot {}", path.display()))?;
    for (index, row) in reader.enumerate() {
        let row = row.context("read snapshot record")?;
        if !accountant.accepts(&row) {
            bail!(
                "snapshot row {} is not {accountant} state: {row:?}",
                index + 1
            );
        }
        match row {
            SnapshotRow::Balance(a) => state.insert_account(&balance_account(&a))?,
            SnapshotRow::Transfer(t) => state.replay.insert((t.chain, t.emitter, t.sequence))?,
            SnapshotRow::Governance(_) => skipped.governance_digests += 1,
            SnapshotRow::ModifyBalance(m) => state.insert_account(&modify_balance_account(&m))?,
            SnapshotRow::TokenBridgeRegistration(r) | SnapshotRow::RelayerChainRegistration(r) => {
                state.insert_account(&chain_registration_account(&r))?;
                state.insert_account(&register_chain_account(&r))?;
            }
            SnapshotRow::TransceiverHub(h) => state.insert_account(&transceiver_hub_account(&h))?,
            SnapshotRow::TransceiverPeer(p) => {
                state.insert_account(&transceiver_peer_account(&p))?
            }
        }
    }
    Ok((state, skipped))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::ComparedState;
    use std::io::Write;

    fn write_snapshot(lines: &[&str]) -> tempfile::NamedTempFile {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        for line in lines {
            writeln!(file, "{line}").unwrap();
        }
        file
    }

    const ACCOUNT: &str = r#"{"kind":"account","chain":1,"token_chain":2,"token_address":"0x00000000000000000000000000000000000000000000000000000000000000aa","balance":"0x0000000000000000000000000000000000000000000000000000000000000007"}"#;
    const TRANSFER: &str = r#"{"kind":"transfer","chain":2,"emitter":"0x00000000000000000000000000000000000000000000000000000000000000ee","sequence":5,"digest":"0x00000000000000000000000000000000000000000000000000000000000000dd","amount":"0x0000000000000000000000000000000000000000000000000000000000000001","token_chain":2,"token_address":"0x00000000000000000000000000000000000000000000000000000000000000aa","recipient_chain":1}"#;

    #[test]
    fn builds_state_from_account_and_transfer_rows() {
        let file = write_snapshot(&[ACCOUNT, TRANSFER]);
        let (state, _) = expected_state_from_snapshot(file.path(), Accountant::Wtt).unwrap();
        assert_eq!(state.state_len(ComparedState::Balances), 1);
        assert_eq!(state.state_len(ComparedState::ReplayMarks), 1);
        let mut token = [0u8; 32];
        token[31] = 0xAA;
        let mut balance = [0u8; 32];
        balance[31] = 0x07;
        assert_eq!(
            state
                .rows(ComparedState::Balances)
                .get(&[&1u16.to_be_bytes()[..], &2u16.to_be_bytes(), &token].concat()),
            Some(&balance.to_vec())
        );
        let mut emitter = [0u8; 32];
        emitter[31] = 0xEE;
        assert_eq!(
            state.replay.iter().collect::<Vec<_>>(),
            vec![(2, emitter, 5)]
        );
    }

    #[test]
    fn rejects_duplicate_rows() {
        let duplicate_account = write_snapshot(&[ACCOUNT, ACCOUNT]);
        assert!(expected_state_from_snapshot(duplicate_account.path(), Accountant::Wtt).is_err());
        let duplicate_transfer = write_snapshot(&[TRANSFER, TRANSFER]);
        assert!(expected_state_from_snapshot(duplicate_transfer.path(), Accountant::Wtt).is_err());
    }
}
