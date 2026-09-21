use accountant_test_harness::mollusk_with_fixtures;
use accountant_test_harness::surfpool::ProgramImage;
use mollusk_svm::Mollusk;
use solana_pubkey::Pubkey;

/// Name of the accountant `.so` under `SBF_OUT_DIR`.
pub const PROGRAM_NAME: &str = "ntt_global_accountant";

/// The accountant's `declare_id!` address.
pub fn program_id() -> Pubkey {
    Pubkey::new_from_array(ntt_global_accountant::ID.to_bytes())
}

pub fn mollusk() -> Mollusk {
    mollusk_with_fixtures(&program_id(), PROGRAM_NAME)
}

/// The accountant from the deploy dir, at its `declare_id!` address.
pub fn accountant_image() -> ProgramImage {
    ProgramImage::from_deploy_dir(PROGRAM_NAME, program_id())
}
