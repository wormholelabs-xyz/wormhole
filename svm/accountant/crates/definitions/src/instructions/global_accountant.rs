//! Global-accountant (WTT) instruction discriminators.

/// Single-byte prefix on the instruction data.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Instruction {
    SubmitObservations = 0,
    ClosePending = 1,
    /// Signed-VAA path through the Verify VAA Shim; applies balances without quorum tracking.
    SubmitVaas = 2,
    /// Token Bridge `RegisterChain` governance: writes the `ChainRegistration` PDA.
    RegisterChain = 3,
    /// Accountant `ModifyBalance` governance: applies a delta to a `BalanceAccount` PDA.
    ModifyBalance = 4,
    /// Accountant `UpgradeContract` governance: upgrades this program from a buffer.
    UpgradeContract = 5,
}

impl Instruction {
    pub const fn from_u8(value: u8) -> Option<Self> {
        match value {
            0 => Some(Self::SubmitObservations),
            1 => Some(Self::ClosePending),
            2 => Some(Self::SubmitVaas),
            3 => Some(Self::RegisterChain),
            4 => Some(Self::ModifyBalance),
            5 => Some(Self::UpgradeContract),
            _ => None,
        }
    }
}
