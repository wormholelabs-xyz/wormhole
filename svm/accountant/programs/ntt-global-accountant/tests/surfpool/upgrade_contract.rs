//! Two-step surfpool e2e for `upgrade_contract`; the flow is
//! `accountant_test_harness::upgrade_e2e`. Run via
//! `just e2e-upgrade-deploy ntt-global-accountant`,
//! `just e2e-upgrade-submit ntt-global-accountant`,
//! `just e2e-upgrade-stop ntt-global-accountant`.

use global_accountant_definitions::NTT_ACCOUNTANT_GOVERNANCE_MODULE;

use crate::common::{accountant_image, NttInstruction, UpgradeE2e};

fn upgrade_e2e() -> UpgradeE2e {
    UpgradeE2e {
        image: accountant_image(),
        module: NTT_ACCOUNTANT_GOVERNANCE_MODULE,
        discriminator: NttInstruction::UpgradeContract as u8,
        scratch_prefix: "ntt-surfpool-upgrade",
        package: env!("CARGO_PKG_NAME"),
    }
}

#[test]
#[ignore = "starts a long-lived surfpool; run via `just e2e-upgrade-deploy ntt-global-accountant`"]
fn deploy_upgradeable_accountant() {
    upgrade_e2e().deploy();
}

#[test]
#[ignore = "needs the surfpool from `just e2e-upgrade-deploy ntt-global-accountant`; run via `just e2e-upgrade-submit ntt-global-accountant`"]
fn submit_upgrade_vaa() {
    upgrade_e2e().submit();
}
