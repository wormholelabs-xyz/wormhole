//! Chains and transceivers the NTT backfill suites share. Chain ids `SOLANA` and `ETHEREUM`
//! come from the harness. Mirrors `programs/ntt-global-accountant/tests/common/actors.rs`.

/// A third chain for cross-registration rows.
pub const POLYGON: u16 = 5;
/// The locking hub, on Solana.
pub const HUB: [u8; 32] = [0x7Bu8; 32];
/// A spoke transceiver on Ethereum.
pub const SPOKE: [u8; 32] = [0x7Au8; 32];
