//! `keccak256` helpers shared across handlers.
//!
//! The on-chain `sol_keccak256` ABI takes a pointer to `&[u8]` fat pointers
//! rather than raw bytes, so callers cannot use the syscall directly without
//! reproducing the slice-of-slices dance.
//!
//! The cfg split exists because pinocchio gates its `syscalls` re-export
//! behind `#[cfg(any(target_os = "solana", target_arch = "bpf"))]` — on the
//! host target the module does not exist at all, so the crate would fail to
//! resolve. Host compilation is required regardless: the mollusk/surfpool
//! integration tests link this crate into host test binaries, and clippy is
//! host-only (the platform-tools toolchain ships no cargo-clippy). The host
//! arm panics rather than silently mis-hashing if ever reached; only the SBF
//! target exercises these helpers for real. `just check` type-checks the SBF
//! arm without a full `cargo build-sbf`.

/// `keccak256(data)` into `result`. Panics on the host target.
pub(crate) fn keccak256(data: &[u8], result: &mut [u8; 32]) {
    #[cfg(any(target_os = "solana", target_arch = "bpf"))]
    {
        let vals: [&[u8]; 1] = [data];
        // SAFETY: pinocchio re-exports the Solana syscall ABI; the runtime
        // reads exactly `val_len` `&[u8]` fat pointers starting at `vals_ptr`.
        unsafe {
            pinocchio::syscalls::sol_keccak256(
                vals.as_ptr() as *const u8,
                vals.len() as u64,
                result.as_mut_ptr(),
            );
        }
    }
    #[cfg(not(any(target_os = "solana", target_arch = "bpf")))]
    {
        let _ = (data, result);
        unreachable!("keccak256 is only available on the SBF target");
    }
}

/// `keccak256(keccak256(body))` — the Wormhole VAA digest convention used by
/// guardian signing and the Verify VAA Shim's `VerifyHash`.
pub(crate) fn double_keccak256(body: &[u8]) -> [u8; 32] {
    let mut inner = [0u8; 32];
    keccak256(body, &mut inner);
    let mut outer = [0u8; 32];
    keccak256(&inner, &mut outer);
    outer
}
