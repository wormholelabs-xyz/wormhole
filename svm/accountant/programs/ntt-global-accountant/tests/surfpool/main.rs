//! Surfpool end-to-end suite for the NTT accountant. Every test is `#[ignore]`; run via
//! `just e2e` or `just e2e-ntt`.

#[path = "../common/mod.rs"]
mod common;
use accountant_test_harness::surfpool as harness;

mod lifecycle;
