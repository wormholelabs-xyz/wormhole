//! Verify VAA Shim program ID and `verify_hash` selector / wire size.

use crate::primitives::Pubkey;

/// Verify VAA Shim program ID (`EFaNWErqAtVWufdNb7yofSHHfWFos843DFpu4JBw24at`),
/// same address on mainnet, devnet, and Tilt localnet. Raw bytes to keep this
/// crate Solana-SDK-free.
pub const VERIFY_VAA_SHIM_PROGRAM_ID: Pubkey = [
    196, 227, 203, 55, 17, 156, 166, 124, 168, 35, 28, 170, 3, 131, 164, 140, 195, 254, 137, 233,
    101, 80, 83, 225, 249, 25, 254, 66, 226, 131, 254, 161,
];

/// Anchor discriminator for the Verify VAA Shim's `verify_hash` — first 8 bytes
/// of `sha256("global:verify_hash")`.
pub const VERIFY_HASH_SELECTOR: [u8; 8] = [22, 152, 160, 69, 241, 148, 14, 124];

/// Wire size of `verify_hash` instruction data: 8-byte selector + 1-byte
/// guardian-set bump + 32-byte digest.
pub const VERIFY_HASH_DATA_LEN: usize = 8 + 1 + 32;
