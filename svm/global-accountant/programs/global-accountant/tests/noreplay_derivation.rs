//! Unit tests for `instructions::noreplay::derive_bucket_pda`.
//!
//! The helper is what `is_marked` uses to reject any caller-supplied bucket
//! account at a non-canonical address. If the derivation drifts away from the
//! upstream `solana_noreplay::pda::BitmapPdaSeeds` scheme, the production
//! `is_marked` would either accept arbitrary buckets or reject the canonical
//! one — both fatal. These tests pin the seed encoding by re-implementing it
//! host-side and asserting the two derivations agree.

use {
    global_accountant::instructions::noreplay::derive_bucket_pda,
    global_accountant_definitions::{
        NOREPLAY_BITS_PER_BUCKET, NOREPLAY_PROGRAM_ID,
    },
    pinocchio::Address,
    solana_pubkey::Pubkey,
};

/// Reference derivation matching `solana_noreplay::pda::BitmapPdaSeeds::new`.
/// Built from `solana_pubkey::Pubkey::find_program_address` rather than the
/// pinocchio one so a transcription error in either side surfaces as a
/// mismatch.
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

#[test]
fn derive_bucket_pda_matches_reference_for_canonical_inputs() {
    // Realistic guardian-set-6 era Solana Token Bridge emitter.
    let authority_bytes = [0x7Au8; 32];
    let authority = Address::from(authority_bytes);
    let chain: u16 = 1;
    let mut emitter = [0u8; 32];
    emitter[31] = 0x11;
    let sequence: u64 = 0x1234_5678_DEAD_BEEF;

    let (ours, ours_bump) = derive_bucket_pda(&authority, chain, &emitter, sequence);
    let (reference, ref_bump) = reference_bucket_pda(&authority_bytes, chain, &emitter, sequence);

    assert_eq!(
        ours.as_array(),
        &reference,
        "derive_bucket_pda must agree with the upstream BitmapPdaSeeds scheme",
    );
    assert_eq!(ours_bump, ref_bump, "canonical bumps must agree");
}

#[test]
fn derive_bucket_pda_is_deterministic() {
    let authority = Address::from([0x99u8; 32]);
    let chain: u16 = 2;
    let emitter = [0xCDu8; 32];
    let sequence: u64 = 42;

    let (first, first_bump) = derive_bucket_pda(&authority, chain, &emitter, sequence);
    let (second, second_bump) = derive_bucket_pda(&authority, chain, &emitter, sequence);
    assert_eq!(first.as_array(), second.as_array());
    assert_eq!(first_bump, second_bump);
}

#[test]
fn derive_bucket_pda_buckets_partition_by_sequence_div_1024() {
    // Sequences in the same bucket (`sequence / 1024` equal) must derive the
    // same PDA. Sequences in different buckets must derive different PDAs.
    let authority = Address::from([0x55u8; 32]);
    let chain: u16 = 4;
    let emitter = [0xEEu8; 32];

    let (same_bucket_lo, _) = derive_bucket_pda(&authority, chain, &emitter, 0);
    let (same_bucket_hi, _) = derive_bucket_pda(&authority, chain, &emitter, 1023);
    let (next_bucket, _) = derive_bucket_pda(&authority, chain, &emitter, 1024);

    assert_eq!(
        same_bucket_lo.as_array(),
        same_bucket_hi.as_array(),
        "sequences 0 and 1023 share bucket 0 — PDAs must match",
    );
    assert_ne!(
        same_bucket_lo.as_array(),
        next_bucket.as_array(),
        "sequence 1024 crosses into bucket 1 — PDA must differ",
    );
}

#[test]
fn derive_bucket_pda_distinguishes_emitter_and_chain() {
    // Sanity: changing chain or emitter (but not sequence) must produce a
    // distinct PDA. Catches any accidental bytes-mixing in the namespace
    // assembly.
    let authority = Address::from([0x33u8; 32]);
    let emitter_a = [0xAAu8; 32];
    let emitter_b = [0xBBu8; 32];

    let (a, _) = derive_bucket_pda(&authority, 1, &emitter_a, 100);
    let (b, _) = derive_bucket_pda(&authority, 2, &emitter_a, 100);
    let (c, _) = derive_bucket_pda(&authority, 1, &emitter_b, 100);

    assert_ne!(a.as_array(), b.as_array(), "chain change must alter PDA");
    assert_ne!(a.as_array(), c.as_array(), "emitter change must alter PDA");
    assert_ne!(b.as_array(), c.as_array(), "different (chain, emitter) → different PDA");
}
