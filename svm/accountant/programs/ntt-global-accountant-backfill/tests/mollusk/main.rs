//! Mollusk suite: in-process runs of the NTT backfill program against the pinned NoReplay
//! binary. `just test` builds the `.so` and runs this binary.

#[path = "../common/mod.rs"]
mod common;

/// The NTT operational program's own body and instruction-data builders, for the
/// cross-`.so` parity test.
#[path = "../../../ntt-global-accountant/tests/common/ix.rs"]
#[allow(dead_code, unused_imports)]
mod ntt_ix;

mod authority;
mod backfill_transceiver_hub;
mod operational_parity;
mod program_id;
mod reused_handlers;
