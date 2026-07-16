//! Wormhole NTT Global Accountant — Solana port (Pinocchio).
//!
//! The product-neutral operational machinery (quorum tracker, signed-VAA
//! backfill, pending cleanup, NoReplay/Shim CPIs, PDA-init, commit-log, hashing,
//! and the zero-copy state layouts) lives in `accountant-operational-core`,
//! deployed under this program's own ID. This crate keeps the NTT-specific
//! governance handlers (`register_relayer_chain`, `modify_balance`) and the
//! entrypoint that wires them up, including the transceiver hub/peer
//! registration handlers. The NTT live-processing path (hub/peer topology,
//! relayer DeliveryInstruction unwrap, amount normalization) is built in a later
//! workstream.

// `no_std` on the SBF target (and `bpf`, so nightly clippy can lint the
// SBF-shaped code — platform-tools ships no clippy).
#![cfg_attr(any(target_os = "solana", target_arch = "bpf"), no_std)]
// `target_os = "solana"` is unknown to the host toolchain.
#![allow(unexpected_cfgs)]

pub mod entrypoint;
pub mod instructions;

pub use global_accountant_definitions as definitions;

// Re-export so the local handlers' `crate::err(...)` calls resolve to the
// single definition in `accountant-operational-core`.
pub use accountant_operational_core::err;

/// Instruction discriminators. Single-byte prefix on the instruction data.
///
/// `0`/`1`/`2` reuse the `accountant-operational-core` operational machinery
/// (`SubmitObservations`/`SubmitVaas` are stubbed pending the NTT transfer
/// flow; `ClosePending` is product-neutral and dispatched directly). `3`/`4`
/// are the NTT governance handlers; `5`/`6` are the transceiver hub/peer
/// registration handlers.
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
