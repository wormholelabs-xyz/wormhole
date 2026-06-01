//! `keccak256` helpers shared across handlers.
//!
//! The on-chain `sol_keccak256` ABI takes a pointer to `&[u8]` fat pointers
//! rather than raw bytes, so callers cannot use the syscall directly without
//! reproducing the slice-of-slices dance. Host-side builds (cargo check
//! outside `cargo build-sbf`) substitute a no-op so the program crate
//! compiles for clippy / type-check; the SBF target is the only path that
//! actually exercises these helpers.

/// `keccak256(data)` into `result`. No-op on the host target.
#[cfg(any(target_os = "solana", target_arch = "bpf"))]
pub(crate) fn keccak256(data: &[u8], result: &mut [u8; 32]) {
    let vals: [&[u8]; 1] = [data];
    // SAFETY: pinocchio re-exports the Solana syscall ABI; the runtime reads
    // exactly `val_len` `&[u8]` fat pointers starting at `vals_ptr`.
    unsafe {
        pinocchio::syscalls::sol_keccak256(
            vals.as_ptr() as *const u8,
            vals.len() as u64,
            result.as_mut_ptr(),
        );
    }
}

#[cfg(not(any(target_os = "solana", target_arch = "bpf")))]
pub(crate) fn keccak256(_data: &[u8], _result: &mut [u8; 32]) {}

/// `keccak256(keccak256(body))` — the Wormhole VAA digest convention used by
/// guardian signing and the Verify VAA Shim's `VerifyHash`.
pub(crate) fn double_keccak256(body: &[u8]) -> [u8; 32] {
    let mut inner = [0u8; 32];
    keccak256(body, &mut inner);
    let mut outer = [0u8; 32];
    keccak256(&inner, &mut outer);
    outer
}
