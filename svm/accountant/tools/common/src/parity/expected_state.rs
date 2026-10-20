//! Expected side of the comparison: the `ChainState` built from the snapshot.
//! Expected account bytes come from `layouts`, the constructors, the encoding
//! the backfill writes.

use anyhow::{bail, ensure, Result};

use crate::accountant::Accountant;
use crate::layouts::{
    balance_layout, chain_registration_layout, modify_balance_layout, register_chain_layout,
    transceiver_hub_layout, transceiver_peer_layout,
};
use crate::parity::state::{ChainState, ReplayMarks, StateKind};
use crate::snapshot::SnapshotRow;

/// Snapshot rows not backfilled.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SkippedRows {
    /// `governance` rows: wormchain digests of governance VAAs. Their NoReplay
    /// bits must stay unset on Solana.
    pub governance: ReplayMarks,
}

/// Expected state of `rows`. Fails on a row of the other accountant and on a duplicate key.
pub fn from_snapshot(
    rows: &[SnapshotRow],
    accountant: Accountant,
) -> Result<(ChainState, SkippedRows)> {
    let mut state = ChainState::default();
    let mut skipped = SkippedRows::default();
    for (index, row) in rows.iter().enumerate() {
        if !accountant.accepts(row) {
            bail!(
                "snapshot row {} is not {accountant} state: {row:?}",
                index + 1
            );
        }
        match row {
            SnapshotRow::Balance(a) => state.insert_account(&balance_layout(a))?,
            SnapshotRow::Transfer(t) => state.replay_marks.insert(t.message_id())?,
            SnapshotRow::Governance(g) => skipped.governance.insert(g.message_id())?,
            SnapshotRow::ModifyBalance(m) => state.insert_account(&modify_balance_layout(m))?,
            SnapshotRow::TokenBridgeRegistration(r) | SnapshotRow::RelayerChainRegistration(r) => {
                state.insert_account(&chain_registration_layout(r))?;
                state.insert_account(&register_chain_layout(r))?;
            }
            SnapshotRow::TransceiverHub(h) => state.insert_account(&transceiver_hub_layout(h))?,
            SnapshotRow::TransceiverPeer(p) => state.insert_account(&transceiver_peer_layout(p))?,
        }
    }
    Ok((state, skipped))
}

/// [`from_snapshot`], and fail on empty Balances or replay marks: a truncated
/// or wrong snapshot against an empty program would compare clean.
pub fn from_snapshot_non_empty(
    rows: &[SnapshotRow],
    accountant: Accountant,
) -> Result<(ChainState, SkippedRows)> {
    let (state, skipped) = from_snapshot(rows, accountant)?;
    for kind in [StateKind::BALANCES, StateKind::ReplayMarks] {
        ensure!(state.len_of(kind) != 0, "snapshot holds no {kind} rows");
    }
    Ok((state, skipped))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::{Address, ChainId, EmitterKey, MessageId, Sequence};
    use crate::parity::state::KeyedKind;
    use crate::snapshot::{BalanceRow, TransferRow};
    use global_accountant_definitions::Uint256;

    fn balance() -> SnapshotRow {
        SnapshotRow::Balance(BalanceRow {
            chain: ChainId(1),
            token_chain: ChainId(2),
            token_address: Address([0xAA; 32]),
            balance: Uint256::from_u128(7),
        })
    }

    fn transfer() -> SnapshotRow {
        SnapshotRow::Transfer(TransferRow {
            chain: ChainId(2),
            emitter: Address([0xEE; 32]),
            sequence: Sequence(5),
            digest: crate::ids::Digest([0xDD; 32]),
            amount: Uint256::from_u128(1),
            token_chain: ChainId(2),
            token_address: Address([0xAA; 32]),
            recipient_chain: ChainId(1),
        })
    }

    #[test]
    fn builds_state_from_balance_and_transfer_rows() {
        let (state, _) = from_snapshot(&[balance(), transfer()], Accountant::Wtt).unwrap();
        assert_eq!(state.len_of(StateKind::BALANCES), 1);
        assert_eq!(state.len_of(StateKind::ReplayMarks), 1);
        let key = [&1u16.to_be_bytes()[..], &2u16.to_be_bytes(), &[0xAA; 32]].concat();
        assert_eq!(
            state.rows(KeyedKind::Balances).get(&key),
            Some(&Uint256::from_u128(7).0.to_vec())
        );
        let want = MessageId {
            emitter: EmitterKey {
                chain: ChainId(2),
                emitter: Address([0xEE; 32]),
            },
            sequence: Sequence(5),
        };
        assert_eq!(state.replay_marks.iter().collect::<Vec<_>>(), vec![want]);
    }

    #[test]
    fn rejects_duplicate_rows() {
        assert!(from_snapshot(&[balance(), balance()], Accountant::Wtt).is_err());
        assert!(from_snapshot(&[transfer(), transfer()], Accountant::Wtt).is_err());
    }

    #[test]
    fn non_empty_rejects_a_snapshot_without_balances_or_marks() {
        assert!(from_snapshot_non_empty(&[balance(), transfer()], Accountant::Wtt).is_ok());
        assert!(from_snapshot_non_empty(&[transfer()], Accountant::Wtt).is_err());
        assert!(from_snapshot_non_empty(&[balance()], Accountant::Wtt).is_err());
    }
}
