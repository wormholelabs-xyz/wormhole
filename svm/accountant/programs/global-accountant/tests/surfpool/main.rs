//! Surfpool end-to-end suite. Every test is `#[ignore]`; run via `just e2e`
//! or the `just e2e-upgrade-*` recipes.

#![allow(clippy::too_many_arguments)]

#[path = "../common/mod.rs"]
mod common;
mod harness;

mod submit_observations;
mod submit_vaas;
mod upgrade_contract;
