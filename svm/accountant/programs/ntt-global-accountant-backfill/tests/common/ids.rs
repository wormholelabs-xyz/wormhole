use accountant_test_harness::mollusk_with_fixtures;
use mollusk_svm::Mollusk;
use solana_pubkey::Pubkey;

/// Name of the backfill `.so` under `SBF_OUT_DIR`.
pub const PROGRAM_NAME: &str = "ntt_global_accountant_backfill";

/// The backfill program's `declare_id!` address, shared with the NTT operational program.
pub fn program_id() -> Pubkey {
    Pubkey::new_from_array(ntt_global_accountant_backfill::ID.to_bytes())
}

pub fn mollusk() -> Mollusk {
    mollusk_with_fixtures(&program_id(), PROGRAM_NAME)
}
