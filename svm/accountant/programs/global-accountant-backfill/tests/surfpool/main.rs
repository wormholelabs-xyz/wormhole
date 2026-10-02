//! Surfpool end-to-end suite for the backfill program. Every test is `#[ignore]`;
//! run the lifecycle via `just e2e-backfill`. The cost probes are operator
//! tooling and need a staged snapshot catalogue: `just e2e-backfill-probe`.

#[path = "../common/mod.rs"]
mod common;
use accountant_test_harness::surfpool as harness;

mod backfill_lifecycle;
mod cost_probe;
mod cost_probe_at_scale;
