//! Checks which `NTT_BACKFILL_AUTHORITY` a *built* `.so` enforces, by running its authority
//! gate inside a mollusk-hosted SBF VM. The gate the artifact carries is what a deploy
//! enforces, and only executing it establishes that; a byte scan of the binary is unreliable.
//! The probe itself lives in `accountant_test_harness::backfill`.
//!
//! - `built_so_accepts_compiled_in_backfill_authority_and_rejects_others` runs under
//!   `just test` against the freshly built artifact.
//! - `verify_so_enforces_specific_operator_authority` is operator-driven: point
//!   `GA_VERIFY_SO` and `GA_EXPECT_AUTHORITY_BASE58` at a deploy artifact and the intended
//!   key, through `just verify-authority <so-path> <base58-pubkey> ntt-global-accountant-backfill`.

use global_accountant_definitions::ntt_global_accountant_backfill::Instruction;
use ntt_global_accountant_backfill::NTT_BACKFILL_AUTHORITY;

mod common;
use common::*;

fn probe() -> ArtifactAuthorityProbe {
    ArtifactAuthorityProbe {
        program_name: PROGRAM_NAME,
        program_id: program_id(),
        compiled_in_authority: NTT_BACKFILL_AUTHORITY,
        authority_var: "NTT_BACKFILL_AUTHORITY",
        discriminator: Instruction::BackfillBalance as u8,
        payload: balance_probe_payload,
    }
}

#[test]
fn built_so_accepts_compiled_in_backfill_authority_and_rejects_others() {
    assert_built_so_enforces_compiled_in_authority(&probe());
}

/// Operator check against a deploy artifact and a production key, both outside source
/// control:
///
/// ```text
/// just verify-authority /path/to/ntt_global_accountant_backfill.so <base58-pubkey> ntt-global-accountant-backfill
/// ```
#[test]
#[ignore = "operator-driven; needs GA_VERIFY_SO + GA_EXPECT_AUTHORITY_BASE58, see the doc comment"]
fn verify_so_enforces_specific_operator_authority() {
    assert_so_enforces_operator_authority(&probe());
}
