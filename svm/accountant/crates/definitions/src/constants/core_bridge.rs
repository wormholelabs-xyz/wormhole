//! Wormhole Core Bridge program ID.

use crate::primitives::Pubkey;

/// Wormhole Core Bridge program ID on Solana mainnet
/// (`worm2ZoG2kUd4vFXhvjh93UUH596ayRfgQ2MgjNMTth`). Raw bytes to keep this
/// crate Solana-SDK-free. Used by `close_pending` to verify the `GuardianSet`
/// account is Core-Bridge-owned before reading it — otherwise a forged
/// "expired" set could permanently DoS a pending PDA.
pub const CORE_BRIDGE_PROGRAM_ID: Pubkey = [
    0x0e, 0x0a, 0x58, 0x9a, 0x41, 0xa5, 0x5f, 0xbd, 0x66, 0xc5, 0x2a, 0x47, 0x5f, 0x2d, 0x92, 0xa6,
    0xd3, 0xdc, 0x9b, 0x47, 0x47, 0x11, 0x4c, 0xb9, 0xaf, 0x82, 0x5a, 0x98, 0xb5, 0x45, 0xd3, 0xce,
];
