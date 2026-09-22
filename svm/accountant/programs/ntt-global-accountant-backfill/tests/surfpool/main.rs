//! Surfpool end-to-end suite for the NTT backfill program. Every test is `#[ignore]`;
//! run the lifecycle via `just e2e-ntt-backfill`.

#[path = "../common/mod.rs"]
mod common;
use accountant_test_harness::surfpool as harness;

mod backfill_lifecycle;
