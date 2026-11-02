//! Surfpool end-to-end suite. Every test is `#[ignore]`; run via `just e2e`
//! or the `just e2e-upgrade-*` recipes.

#![allow(clippy::too_many_arguments)]

#[path = "../common/mod.rs"]
mod common;
use accountant_test_harness::surfpool as harness;

mod modify_balance;
mod register_chain;
mod submit_observations;
mod submit_vaas;
mod submit_vaas_max_payload;
mod upgrade_contract;
