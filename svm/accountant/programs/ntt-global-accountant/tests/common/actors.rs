//! Chains and transceivers the NTT suites share. Chain ids `SOLANA` and `ETHEREUM` come from
//! the harness.

/// A third chain for cross-registration rows.
pub const POLYGON: u16 = 5;
/// The locking hub, on Solana.
pub const HUB: [u8; 32] = [0x7Bu8; 32];
/// A spoke transceiver on Ethereum.
pub const SPOKE: [u8; 32] = [0x7Au8; 32];
/// Ethereum's Standard Relayer emitter.
pub const RELAYER: [u8; 32] = [0x7Eu8; 32];
/// Another transceiver, on Polygon unless a row says otherwise.
pub const OTHER: [u8; 32] = [0x7Cu8; 32];
