//! Deterministic backfill operator keys. Each backfill program pins one pubkey at compile
//! time from its own `env!` variable; the justfile sets those to the keys below for test
//! artifacts.

use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use solana_signer::Signer;

/// Signer the `global-accountant-backfill` test artifact accepts. Its pubkey is the
/// justfile's `TEST_BACKFILL_AUTHORITY`, passed as `BACKFILL_AUTHORITY` at build time.
pub fn test_authority_keypair() -> Keypair {
    Keypair::new_from_array([1u8; 32])
}

pub fn test_authority_pubkey() -> Pubkey {
    test_authority_keypair().pubkey()
}

/// Signer the `ntt-global-accountant-backfill` test artifact accepts. Its pubkey is the
/// justfile's `TEST_NTT_BACKFILL_AUTHORITY`, passed as `NTT_BACKFILL_AUTHORITY` at build
/// time. Distinct from the WTT key so cross-program authority isolation is testable.
pub fn ntt_test_authority_keypair() -> Keypair {
    Keypair::new_from_array([2u8; 32])
}

pub fn ntt_test_authority_pubkey() -> Pubkey {
    ntt_test_authority_keypair().pubkey()
}
