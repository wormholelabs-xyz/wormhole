//! Instruction discriminators for the global-accountant (WTT) program's
//! dispatch table.

/// Instruction discriminators. Single-byte prefix on the instruction data.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Instruction {
    SubmitObservations = 0,
    ClosePending = 1,
    /// Permissionless signed-VAA backfill via the Verify VAA Shim CPI; applies
    /// balance effects directly, bypassing the quorum tracker.
    SubmitVaas = 2,
    /// Token Bridge `RegisterChain` governance handler: verifies the VAA and
    /// initialises/updates the canonical `ChainRegistration` PDA.
    RegisterChain = 3,
    /// Accountant `ModifyBalance` governance handler: verifies the VAA and
    /// applies an Add/Subtract delta to the canonical `BalanceAccount` PDA.
    ModifyBalance = 4,
}

impl Instruction {
    pub const fn from_u8(value: u8) -> Option<Self> {
        match value {
            0 => Some(Self::SubmitObservations),
            1 => Some(Self::ClosePending),
            2 => Some(Self::SubmitVaas),
            3 => Some(Self::RegisterChain),
            4 => Some(Self::ModifyBalance),
            _ => None,
        }
    }
}
