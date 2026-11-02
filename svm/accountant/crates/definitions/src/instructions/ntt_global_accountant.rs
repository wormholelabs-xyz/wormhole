//! NTT global-accountant instruction discriminators. Namespaced: both programs name their enum
//! `Instruction`. Numbering mirrors the WTT program for the shared instructions.

/// Single-byte prefix on the instruction data.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Instruction {
    /// One guardian's signed NTT observation; commits at quorum.
    SubmitObservations = 0,
    /// Close a `PendingObservations` PDA to reclaim rent.
    ClosePending = 1,
    /// Signed NTT transfer VAA: books the transfer under the sender's hub.
    SubmitVaas = 2,
    /// Standard Relayer `RegisterChain` governance: writes the relayer `ChainRegistration` PDA.
    RegisterRelayerChain = 3,
    /// NTT accountant `ModifyBalance` governance: applies a delta to a `BalanceAccount` PDA.
    ModifyBalance = 4,
    /// Transceiver info (Locking mode): writes the self-referential `TransceiverHub` PDA.
    RegisterHub = 5,
    /// Transceiver peer registration: writes the `TransceiverPeer` PDA after the hub check.
    RegisterPeer = 6,
    /// NTT accountant `UpgradeContract` governance: upgrades this program from a buffer.
    UpgradeContract = 7,
}

impl Instruction {
    pub const fn from_u8(value: u8) -> Option<Self> {
        match value {
            0 => Some(Self::SubmitObservations),
            1 => Some(Self::ClosePending),
            2 => Some(Self::SubmitVaas),
            3 => Some(Self::RegisterRelayerChain),
            4 => Some(Self::ModifyBalance),
            5 => Some(Self::RegisterHub),
            6 => Some(Self::RegisterPeer),
            7 => Some(Self::UpgradeContract),
            _ => None,
        }
    }
}
