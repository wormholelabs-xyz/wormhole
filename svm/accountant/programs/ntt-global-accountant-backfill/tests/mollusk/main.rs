//! Mollusk suite: in-process runs of the NTT backfill program against the pinned NoReplay
//! binary. `just test` builds the `.so` and runs this binary.

#[path = "../common/mod.rs"]
mod common;

mod authority;
mod program_id;
mod reused_handlers;
