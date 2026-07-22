//! Unit tests for `noreplay::derive_bucket_pda`, pinning its seed encoding
//! against a host-side reimplementation of `solana_noreplay::pda::BitmapPdaSeeds`.
//! The helper now lives in the shared `accountant-operational-core` crate.

use {
    accountant_operational_core::instructions::noreplay::derive_bucket_pda,
    global_accountant_definitions::{NOREPLAY_BITS_PER_BUCKET, NOREPLAY_PROGRAM_ID},
    solana_pubkey::Pubkey,
    // `derive_bucket_pda` takes `anchor_lang::solana_program::pubkey::Pubkey`,
    // resolved to the `3.x` line — a separate crate instance from this test's
    // own `solana_pubkey` (`4.1`, used below only for the reference
    // derivation). See the dependency comment in Cargo.toml.
    solana_pubkey_v3::Pubkey as CorePubkey,
};

/// Reference derivation matching `solana_noreplay::pda::BitmapPdaSeeds::new`,
/// built via `solana_pubkey` (vs. the program's pinocchio impl) to catch
/// transcription errors on either side.
fn reference_bucket_pda(
    authority: &[u8; 32],
    chain: u16,
    emitter: &[u8; 32],
    sequence: u64,
) -> ([u8; 32], u8) {
    let mut namespace = [0u8; 2 + 32];
    namespace[..2].copy_from_slice(&chain.to_be_bytes());
    namespace[2..].copy_from_slice(emitter);
    let mid = namespace.len().min(32);
    let bucket_index = (sequence / NOREPLAY_BITS_PER_BUCKET).to_le_bytes();
    let (pubkey, bump) = Pubkey::find_program_address(
        &[
            authority,
            &namespace[..mid],
            &namespace[mid..],
            &bucket_index,
        ],
        &Pubkey::new_from_array(NOREPLAY_PROGRAM_ID),
    );
    (pubkey.to_bytes(), bump)
}

/// `derive_bucket_pda` agrees with the reference derivation (address + bump).
#[test]
fn derive_bucket_pda_matches_reference_for_canonical_inputs() {
    let authority_bytes = [0x7Au8; 32];
    let authority = CorePubkey::new_from_array(authority_bytes);
    let chain: u16 = 1;
    let mut emitter = [0u8; 32];
    emitter[31] = 0x11;
    let sequence: u64 = 0x1234_5678_DEAD_BEEF;

    let (ours, ours_bump) = derive_bucket_pda(&authority, chain, &emitter, sequence);
    let (reference, ref_bump) = reference_bucket_pda(&authority_bytes, chain, &emitter, sequence);

    assert_eq!(
        &ours.to_bytes(),
        &reference,
        "derive_bucket_pda must agree with the upstream BitmapPdaSeeds scheme",
    );
    assert_eq!(ours_bump, ref_bump, "canonical bumps must agree");
}

/// Pin the `sequence / 1024` bucket math at the bucket boundaries (`0`, `1023`,
/// `1024`) and the extremes (`u64::MAX`), plus a non-1 chain so the chain bytes
/// participate in the namespace seed. A single canonical case cannot catch an
/// off-by-one in the bucket-index division or a missing chain prefix; these do.
#[test]
fn derive_bucket_pda_matches_reference_at_bucket_boundaries() {
    let authority_bytes = [0x7Au8; 32];
    let authority = CorePubkey::new_from_array(authority_bytes);
    let mut emitter = [0u8; 32];
    emitter[0] = 0xAB;
    emitter[31] = 0x11;

    // (chain, sequence) cases. Boundary sequences straddle the 1024-bit bucket
    // edge; the chain values exercise both chain 1 and a multi-byte chain id.
    let cases: [(u16, u64); 8] = [
        (1, 0),
        (1, 1023),
        (1, 1024),
        (1, 1025),
        (1, u64::MAX),
        (56, 1023), // non-1 chain at a bucket edge
        (56, 1024),
        (56, u64::MAX),
    ];

    for (chain, sequence) in cases {
        let (ours, ours_bump) = derive_bucket_pda(&authority, chain, &emitter, sequence);
        let (reference, ref_bump) =
            reference_bucket_pda(&authority_bytes, chain, &emitter, sequence);
        assert_eq!(
            &ours.to_bytes(),
            &reference,
            "bucket PDA mismatch at chain={chain} sequence={sequence}",
        );
        assert_eq!(
            ours_bump, ref_bump,
            "bucket bump mismatch at chain={chain} sequence={sequence}",
        );
    }

    // The boundary actually crosses a bucket: seq 1023 and 1024 must derive
    // different PDAs (different bucket index), while 1024 and 1025 share one.
    let (b1023, _) = derive_bucket_pda(&authority, 1, &emitter, 1023);
    let (b1024, _) = derive_bucket_pda(&authority, 1, &emitter, 1024);
    let (b1025, _) = derive_bucket_pda(&authority, 1, &emitter, 1025);
    assert_ne!(
        &b1023.to_bytes(),
        &b1024.to_bytes(),
        "seq 1023 and 1024 fall in different buckets",
    );
    assert_eq!(
        &b1024.to_bytes(),
        &b1025.to_bytes(),
        "seq 1024 and 1025 share a bucket",
    );
}
