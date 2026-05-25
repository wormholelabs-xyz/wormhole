//! NoReplay integration. Mock + real branches gated by the `mock-noreplay`
//! feature mirroring `mock-vaa`'s shape in `close_digest.rs`.
//!
//! The mock branch is a single-byte sentinel in a caller-supplied account
//! (`0x01` = marked, anything else = unmarked). The real branch will CPI into
//! `solana-noreplay` per `accountant-migration-noreplay-integration.md`; that
//! integration is Phase 2.3 and currently behind a `compile_error!` fence.
//!
//! Both `submit_observations::process` (pre-check + commit-time flip) and
//! `close_pending::process` (trigger-(b) check) use these helpers, which is
//! why they live in their own module rather than inside `submit_observations`.

use pinocchio::{AccountView, ProgramResult};

/// In-memory replay-protection sentinel byte. The mock NoReplay bucket is a
/// single-byte account: `0x01` = "this (chain, emitter, sequence) is already
/// accounted-for", anything else = "free to commit". The host test creates
/// the account at the canonical bucket address and toggles this byte to
/// verify the production path's pre-check fires.
#[cfg(feature = "mock-noreplay")]
const MOCK_NOREPLAY_MARKED: u8 = 0x01;

#[cfg(feature = "mock-noreplay")]
pub fn is_marked(
    bucket: &AccountView,
    _chain: u16,
    _emitter: &[u8; 32],
    _sequence: u64,
) -> Result<bool, pinocchio::error::ProgramError> {
    let data = bucket.try_borrow()?;
    if data.is_empty() {
        return Ok(false);
    }
    Ok(data[0] == MOCK_NOREPLAY_MARKED)
}

#[cfg(feature = "mock-noreplay")]
pub fn mark_used(
    _payer: &AccountView,
    bucket: &mut AccountView,
    _chain: u16,
    _emitter: &[u8; 32],
    _sequence: u64,
) -> ProgramResult {
    let mut data = bucket.try_borrow_mut()?;
    if data.is_empty() {
        return Err(crate::err(
            crate::definitions::GlobalAccountantError::NoReplayCpiFailed,
        ));
    }
    data[0] = MOCK_NOREPLAY_MARKED;
    Ok(())
}

// Phase 2.3 will replace this `compile_error!` with the real CPI into
// `solana-noreplay`. Mirrors the historical `mock-vaa` fence in
// `close_digest.rs` before the real Shim CPI landed.
#[cfg(not(feature = "mock-noreplay"))]
compile_error!(
    "submit_observations production path requires the real solana-noreplay \
     CPI (Phase 2.3); for now build with \
     `--features mock-noreplay,mock-vaa,test-only-open-digest`"
);

// The `compile_error!` above already poisons this branch; the function bodies
// below exist only so `rustc` can finish name resolution before emitting the
// fence's error. They return `NoReplayCpiFailed` rather than panicking — the
// constraint "no `unreachable!()` in new code paths" demands a fallible return
// even on unreachable branches. In practice the compile_error fires first.
#[cfg(not(feature = "mock-noreplay"))]
pub fn is_marked(
    _bucket: &AccountView,
    _chain: u16,
    _emitter: &[u8; 32],
    _sequence: u64,
) -> Result<bool, pinocchio::error::ProgramError> {
    Err(crate::err(
        crate::definitions::GlobalAccountantError::NoReplayCpiFailed,
    ))
}

#[cfg(not(feature = "mock-noreplay"))]
pub fn mark_used(
    _payer: &AccountView,
    _bucket: &mut AccountView,
    _chain: u16,
    _emitter: &[u8; 32],
    _sequence: u64,
) -> ProgramResult {
    Err(crate::err(
        crate::definitions::GlobalAccountantError::NoReplayCpiFailed,
    ))
}
