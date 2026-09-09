//! Mollusk suite: in-process runs of the backfill program against the pinned
//! NoReplay binary. `just test` builds the `.so` and runs this binary.

#[path = "../common/mod.rs"]
mod common;

mod backfill_balance;
mod backfill_modify_balance;
mod backfill_noreplay;
