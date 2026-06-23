//! `keccak256` helpers shared across handlers.
//!
//! The SBF arm calls the syscall; the host arm panics. Host compilation is
//! still required (mollusk/surfpool test binaries and clippy are host-only),
//! and pinocchio's `syscalls` re-export does not exist on the host target, so
//! the cfg split is mandatory.

/// `keccak256(data)` into `result`. Panics on the host target.
pub(crate) fn keccak256(data: &[u8], result: &mut [u8; 32]) {
    #[cfg(any(target_os = "solana", target_arch = "bpf"))]
    {
        let vals: [&[u8]; 1] = [data];
        // SAFETY: the runtime reads exactly `val_len` `&[u8]` fat pointers
        // starting at `vals_ptr`.
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
pub fn double_keccak256(body: &[u8]) -> [u8; 32] {
    let mut inner = [0u8; 32];
    keccak256(body, &mut inner);
    let mut outer = [0u8; 32];
    keccak256(&inner, &mut outer);
    outer
}

/// `keccak256` over the concatenation of `parts`, in a single syscall. Panics on
/// the host target.
fn keccak256_parts(parts: &[&[u8]], result: &mut [u8; 32]) {
    #[cfg(any(target_os = "solana", target_arch = "bpf"))]
    {
        // SAFETY: the runtime reads exactly `parts.len()` `&[u8]` fat pointers
        // starting at `parts.as_ptr()` and hashes their concatenation. Same ABI
        // as the single-slice `keccak256`, with N entries instead of one.
        unsafe {
            pinocchio::syscalls::sol_keccak256(
                parts.as_ptr() as *const u8,
                parts.len() as u64,
                result.as_mut_ptr(),
            );
        }
    }
    #[cfg(not(any(target_os = "solana", target_arch = "bpf")))]
    {
        let _ = (parts, result);
        unreachable!("keccak256 is only available on the SBF target");
    }
}

/// `keccak256(prefix ‖ tx_hash ‖ body)` — the guardian observation signing
/// digest. Mirrors the node's `vaa.MessageSigningDigest(prefix, observation)`
/// (`sdk/vaa/structs.go`): a *single* keccak over the domain-separation prefix
/// followed by the observation bytes, where the observation is `tx_hash` (the
/// source-chain transaction id, audit-only) concatenated with the canonical VAA
/// `body`.
///
/// This is deliberately NOT [`double_keccak256`]: that produces the VAA-body
/// digest, which keys dedup/quorum state and verifies VAA signatures on the
/// `submit_vaas` path. The prefixed single-keccak here is the *observation*
/// attestation domain — the two must stay distinct so an observation signature
/// is never interchangeable with a VAA signature.
pub fn observation_signing_digest(prefix: &[u8], tx_hash: &[u8; 32], body: &[u8]) -> [u8; 32] {
    let mut out = [0u8; 32];
    keccak256_parts(&[prefix, tx_hash, body], &mut out);
    out
}
