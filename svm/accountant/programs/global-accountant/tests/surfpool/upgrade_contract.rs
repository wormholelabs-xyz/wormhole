//! Two-step surfpool e2e for `upgrade_contract`; the flow is
//! `accountant_test_harness::upgrade_e2e`. Run via `just e2e-upgrade-deploy`,
//! `just e2e-upgrade-submit`, `just e2e-upgrade-stop`.

use global_accountant_definitions::{Instruction, ACCOUNTANT_GOVERNANCE_MODULE};

use crate::common::{accountant_image, UpgradeE2e};

fn upgrade_e2e() -> UpgradeE2e {
    UpgradeE2e {
        image: accountant_image(),
        module: ACCOUNTANT_GOVERNANCE_MODULE,
        discriminator: Instruction::UpgradeContract as u8,
        scratch_prefix: "ga-surfpool-upgrade",
        package: env!("CARGO_PKG_NAME"),
    }
}

#[test]
#[ignore = "starts a long-lived surfpool; run via `just e2e-upgrade-deploy`"]
fn deploy_upgradeable_accountant() {
    upgrade_e2e().deploy();
}

#[test]
#[ignore = "needs the surfpool from `just e2e-upgrade-deploy`; run via `just e2e-upgrade-submit`"]
fn submit_upgrade_vaa() {
    upgrade_e2e().submit();
}
