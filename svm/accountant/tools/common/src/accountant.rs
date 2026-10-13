//! Accountant: the WTT or the NTT accountant. Selects the program id and the
//! snapshot kinds a run accepts.

use solana_pubkey::Pubkey;

use global_accountant_definitions::{
    GLOBAL_ACCOUNTANT_PROGRAM_ID, NTT_GLOBAL_ACCOUNTANT_PROGRAM_ID,
};

use crate::snapshot::SnapshotRow;

#[derive(
    Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "lowercase")]
pub enum Accountant {
    Wtt,
    Ntt,
}

impl Accountant {
    /// Address in the backfill `.so`'s `declare_id!`. Anchor aborts on any other runtime id.
    pub fn program_id(self) -> Pubkey {
        match self {
            Self::Wtt => Pubkey::new_from_array(GLOBAL_ACCOUNTANT_PROGRAM_ID),
            Self::Ntt => Pubkey::new_from_array(NTT_GLOBAL_ACCOUNTANT_PROGRAM_ID),
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
