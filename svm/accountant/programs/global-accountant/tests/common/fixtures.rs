use accountant_test_harness::surfpool::ProgramImage;

use super::ids::program_id;

/// Name of the accountant `.so` under `SBF_OUT_DIR`.
pub const PROGRAM_NAME: &str = "global_accountant";

/// The accountant from the deploy dir, at its `declare_id!` address.
pub fn accountant_image() -> ProgramImage {
    ProgramImage::from_deploy_dir(PROGRAM_NAME, program_id())
}
