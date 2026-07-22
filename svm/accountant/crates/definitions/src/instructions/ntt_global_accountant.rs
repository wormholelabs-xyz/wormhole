//! Instruction discriminators for the ntt-global-accountant (NTT) program's
//! dispatch table.
//!
//! Kept in its own submodule (rather than flattened to the crate root like
//! [`super::global_accountant`]) because both programs' `Instruction` enums
//! share the name; namespacing here avoids the clash. `ntt-global-accountant`
//! is this submodule's first consumer — see the module doc on
//! [`super`] for the placement rationale.

/// Instruction discriminators. Single-byte prefix on the instruction data.
///
/// `0`/`1`/`2` reuse the `accountant-operational-core` operational machinery
/// (`SubmitObservations`/`SubmitVaas` run the NTT transfer flow on the
/// committing branch; `ClosePending` is product-neutral and dispatched
/// directly). `3`/`4` are the NTT governance handlers; `5`/`6` are the
/// transceiver hub/peer registration handlers.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Instruction {
    SubmitObservations = 0,
    ClosePending = 1,
    SubmitVaas = 2,
    /// `WormholeRelayer` `RegisterChain` governance handler: verifies the VAA
    /// and initialises/updates the canonical `RelayerChainRegistration` PDA.
    RegisterRelayerChain = 3,
    /// NTT `ModifyBalance` governance handler: verifies the VAA and applies an
    /// Add/Subtract delta to the canonical `BalanceAccount` PDA.
    ModifyBalance = 4,
    /// NTT transceiver-hub registration (`INFO_PREFIX`, Locking-mode only):
    /// verifies the VAA and writes the canonical `TransceiverHub` PDA pointing
    /// the transceiver at itself.
    RegisterHub = 5,
    /// NTT transceiver-peer registration (`PEER_INFO_PREFIX`): verifies the VAA,
    /// validates the bidirectional hub match, and writes the canonical
    /// `TransceiverPeer` PDA.
    RegisterPeer = 6,
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
            _ => None,
        }
    }
}
