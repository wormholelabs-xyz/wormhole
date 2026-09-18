//! NTT global-accountant instruction discriminators. Namespaced: both programs name their enum
//! `Instruction`. Numbering mirrors the WTT program for the shared instructions.

/// Single-byte prefix on the instruction data. `0`, `2`, `5` and `6` are taken by
/// `submit_observations`, `submit_vaas`, `register_hub` and `register_peer`.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Instruction {
    /// Close a `PendingObservations` PDA to reclaim rent.
    ClosePending = 1,
    /// Standard Relayer `RegisterChain` governance: writes the relayer `ChainRegistration` PDA.
    RegisterRelayerChain = 3,
    /// NTT accountant `ModifyBalance` governance: applies a delta to a `BalanceAccount` PDA.
    ModifyBalance = 4,
    /// NTT accountant `UpgradeContract` governance: upgrades this program from a buffer.
    UpgradeContract = 7,
}

impl Instruction {
    pub const fn from_u8(value: u8) -> Option<Self> {
        match value {
            1 => Some(Self::ClosePending),
            3 => Some(Self::RegisterRelayerChain),
            4 => Some(Self::ModifyBalance),
            7 => Some(Self::UpgradeContract),
            _ => None,
        }
    }
}
