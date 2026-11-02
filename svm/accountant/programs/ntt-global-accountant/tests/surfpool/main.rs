//! Surfpool end-to-end suite for the NTT accountant. Every test is `#[ignore]`; run via
//! `just e2e` or `just e2e-ntt`. The upgrade_contract module is a two-step flow with the
//! `just e2e-upgrade-* ntt-global-accountant` recipes.

#[path = "../common/mod.rs"]
mod common;
use accountant_test_harness::surfpool as harness;

mod lifecycle;
mod upgrade_contract;
