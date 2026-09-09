//! Surfpool end-to-end suite. Every test is `#[ignore]`; run via
//! `just e2e-backfill`.

#[path = "../common/mod.rs"]
mod common;
mod harness;

mod backfill_lifecycle;
mod cost_probe;
mod cost_probe_at_scale;
