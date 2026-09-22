//! Surfpool end-to-end suite for the NTT backfill program. Every test is `#[ignore]`; run
//! the lifecycle via `just e2e-ntt-backfill` and the cutover rehearsal via
//! `just e2e-ntt-cutover`.

#[path = "../common/mod.rs"]
mod common;
use accountant_test_harness::surfpool as harness;

/// The NTT operational program's instruction-data builders, for the post-cutover transfer.
#[path = "../../../ntt-global-accountant/tests/common/ix.rs"]
#[allow(dead_code, unused_imports)]
mod ntt_ix;

mod backfill_lifecycle;
mod cutover;
