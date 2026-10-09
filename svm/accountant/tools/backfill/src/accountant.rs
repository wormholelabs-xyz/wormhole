//! Backfill target: the WTT or the NTT accountant. Selects the program id, the
//! operator authority, the instruction discriminators and the snapshot kinds a
//! run accepts.

use solana_pubkey::Pubkey;

use global_accountant_backfill::{Instruction as WttIx, BACKFILL_AUTHORITY};
use global_accountant_definitions::{
    GLOBAL_ACCOUNTANT_PROGRAM_ID, NTT_GLOBAL_ACCOUNTANT_PROGRAM_ID,
};
use ntt_global_accountant_backfill::{Instruction as NttIx, NTT_BACKFILL_AUTHORITY};

use crate::snapshot::SnapshotRow;

#[derive(
    Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "lowercase")]
pub enum Accountant {
    Wtt,
    Ntt,
}

/// One discriminator per backfill instruction. The NTT program numbers its shared
/// instructions like the WTT program; [`Accountant::discriminators`] reads both enums.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Discriminators {
    pub noreplay: u8,
    pub balance: u8,
    pub modify_balance: u8,
    pub chain_registration: u8,
    /// `Some` on NTT only: the transceiver maps are NTT state.
    pub transceiver_hub: Option<u8>,
    pub transceiver_peer: Option<u8>,
}

impl Accountant {
    /// Address in the backfill `.so`'s `declare_id!`. Anchor aborts on any other runtime id.
    pub fn program_id(self) -> Pubkey {
        match self {
            Self::Wtt => Pubkey::new_from_array(GLOBAL_ACCOUNTANT_PROGRAM_ID),
            Self::Ntt => Pubkey::new_from_array(NTT_GLOBAL_ACCOUNTANT_PROGRAM_ID),
        }
    }

    /// Signer the backfill `.so` requires on every instruction.
    pub fn authority(self) -> Pubkey {
        match self {
            Self::Wtt => Pubkey::new_from_array(BACKFILL_AUTHORITY),
            Self::Ntt => Pubkey::new_from_array(NTT_BACKFILL_AUTHORITY),
        }
    }

    pub fn discriminators(self) -> Discriminators {
        match self {
            Self::Wtt => Discriminators {
                noreplay: WttIx::BackfillNoReplay as u8,
                balance: WttIx::BackfillBalance as u8,
                modify_balance: WttIx::BackfillModifyBalance as u8,
                chain_registration: WttIx::BackfillChainRegistration as u8,
                transceiver_hub: None,
                transceiver_peer: None,
            },
            Self::Ntt => Discriminators {
                noreplay: NttIx::BackfillNoReplay as u8,
                balance: NttIx::BackfillBalance as u8,
                modify_balance: NttIx::BackfillModifyBalance as u8,
                chain_registration: NttIx::BackfillRelayerChainRegistration as u8,
                transceiver_hub: Some(NttIx::BackfillTransceiverHub as u8),
                transceiver_peer: Some(NttIx::BackfillTransceiverPeer as u8),
            },
        }
    }

    /// Whether `row` belongs to this accountant's snapshot. Token Bridge
    /// registrations are WTT only; relayer registrations and the transceiver
    /// maps are NTT only.
    pub fn accepts(self, row: &SnapshotRow) -> bool {
        match row {
            SnapshotRow::Transfer(_)
            | SnapshotRow::Balance(_)
            | SnapshotRow::ModifyBalance(_)
            | SnapshotRow::Governance(_) => true,
            SnapshotRow::TokenBridgeRegistration(_) => self == Self::Wtt,
            SnapshotRow::RelayerChainRegistration(_)
            | SnapshotRow::TransceiverHub(_)
            | SnapshotRow::TransceiverPeer(_) => self == Self::Ntt,
        }
    }
}

impl std::fmt::Display for Accountant {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Wtt => "wtt",
            Self::Ntt => "ntt",
        })
    }
}
