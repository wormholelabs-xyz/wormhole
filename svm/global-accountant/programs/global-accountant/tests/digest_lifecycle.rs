//! Integration tests for the DigestAccount lifecycle (open + close).
//!
//! Written test-first per `.claude/tasks/accountant-migration.md` §14.
//! Each test drives the on-chain program through mollusk-svm; the program
//! itself must be built via `cargo build-sbf` before `cargo test` runs.
//! The companion script `scripts/build-and-test.sh` (or `make test`) handles
//! this end-to-end.

use {
    global_accountant_definitions::{
        DigestAccountLayout, GlobalAccountantError, Instruction as IxDiscriminator,
        DIGEST_SEED_PREFIX,
    },
    mollusk_svm::{program::keyed_account_for_system_program, result::ProgramResult, Mollusk},
    solana_account::Account,
    solana_instruction::{AccountMeta, Instruction},
    solana_pubkey::Pubkey,
};

const PROGRAM_NAME: &str = "global_accountant";

fn program_id() -> Pubkey {
    // Fixed program id so PDA derivation in the test matches the program's
    // on-chain view. The actual bytes are irrelevant for mollusk's purposes.
    Pubkey::new_from_array([7u8; 32])
}

fn mollusk() -> Mollusk {
    Mollusk::new(&program_id(), PROGRAM_NAME)
}

fn system_program_id() -> Pubkey {
    keyed_account_for_system_program().0
}

fn derive_digest_pda(chain: u16, emitter: &[u8; 32], sequence: u64) -> (Pubkey, u8) {
    let chain_be = chain.to_be_bytes();
    let sequence_be = sequence.to_be_bytes();
    Pubkey::find_program_address(
        &[DIGEST_SEED_PREFIX, &chain_be, emitter, &sequence_be],
        &program_id(),
    )
}

fn open_digest_ix_data(
    chain: u16,
    emitter: &[u8; 32],
    sequence: u64,
    digest: &[u8; 32],
    guardian_set_index: u32,
    bump: u8,
) -> Vec<u8> {
    let mut data = Vec::with_capacity(1 + 79);
    data.push(IxDiscriminator::OpenDigest as u8);
    data.extend_from_slice(&chain.to_be_bytes());
    data.extend_from_slice(emitter);
    data.extend_from_slice(&sequence.to_be_bytes());
    data.extend_from_slice(digest);
    data.extend_from_slice(&guardian_set_index.to_le_bytes());
    data.push(bump);
    data
}

fn close_digest_ix_data(mock_vaa_digest: &[u8; 32]) -> Vec<u8> {
    let mut data = Vec::with_capacity(1 + 32);
    data.push(IxDiscriminator::CloseDigest as u8);
    data.extend_from_slice(mock_vaa_digest);
    data
}

fn lifecycle_inputs() -> (u16, [u8; 32], u64, [u8; 32], u32) {
    let chain: u16 = 1;
    let mut emitter = [0u8; 32];
    emitter[31] = 0x42;
    let sequence: u64 = 0x0123_4567_89ab_cdef;
    let mut digest = [0u8; 32];
    for (i, byte) in digest.iter_mut().enumerate() {
        *byte = i as u8;
    }
    let guardian_set_index: u32 = 3;
    (chain, emitter, sequence, digest, guardian_set_index)
}

fn payer_account(lamports: u64) -> Account {
    Account {
        lamports,
        data: vec![],
        owner: system_program_id(),
        executable: false,
        rent_epoch: 0,
    }
}

fn uninitialised_pda_account() -> Account {
    Account {
        lamports: 0,
        data: vec![],
        owner: system_program_id(),
        executable: false,
        rent_epoch: 0,
    }
}

#[test]
fn open_then_close_round_trip() {
    let mollusk = mollusk();
    let (chain, emitter, sequence, digest, guardian_set_index) = lifecycle_inputs();
    let payer = Pubkey::new_from_array([1u8; 32]);
    let (pda, bump) = derive_digest_pda(chain, &emitter, sequence);

    // -- Open ---------------------------------------------------------------
    let open_ix = Instruction::new_with_bytes(
        program_id(),
        &open_digest_ix_data(chain, &emitter, sequence, &digest, guardian_set_index, bump),
        vec![
            AccountMeta::new(payer, true),
            AccountMeta::new(pda, false),
            AccountMeta::new_readonly(system_program_id(), false),
        ],
    );

    let open_accounts = vec![
        (payer, payer_account(10_000_000_000)),
        (pda, uninitialised_pda_account()),
        keyed_account_for_system_program(),
    ];

    let open_result = mollusk.process_instruction(&open_ix, &open_accounts);
    assert!(
        matches!(open_result.program_result, ProgramResult::Success),
        "open_digest failed: {:?}",
        open_result.program_result
    );

    let (_, pda_after_open) = open_result
        .resulting_accounts
        .iter()
        .find(|(k, _)| *k == pda)
        .expect("PDA in resulting accounts");
    assert_eq!(pda_after_open.owner, program_id(), "PDA owner");
    assert_eq!(
        pda_after_open.data.len(),
        DigestAccountLayout::LEN,
        "PDA data length"
    );

    let stored: &DigestAccountLayout = bytemuck::from_bytes(&pda_after_open.data);
    assert_eq!(stored.chain, chain);
    assert_eq!(stored.emitter, emitter);
    assert_eq!(stored.sequence, sequence);
    assert_eq!(stored.digest, digest);
    assert_eq!(stored.payer, payer.to_bytes());
    assert_eq!(stored.guardian_set_index, guardian_set_index);
    // Slot is set by the runtime; just assert it was written.
    assert_ne!(stored.quorum_at_slot, u64::MAX);

    let (_, payer_after_open) = open_result
        .resulting_accounts
        .iter()
        .find(|(k, _)| *k == payer)
        .expect("payer in resulting accounts");
    let payer_paid = 10_000_000_000_u64.saturating_sub(payer_after_open.lamports);
    assert!(payer_paid > 0, "payer should have funded rent");
    assert_eq!(
        payer_paid, pda_after_open.lamports,
        "rent debit must equal PDA balance"
    );

    // -- Close --------------------------------------------------------------
    let mock_vaa_first_32 = digest; // matches the stored digest
    let close_ix = Instruction::new_with_bytes(
        program_id(),
        &close_digest_ix_data(&mock_vaa_first_32),
        vec![
            AccountMeta::new_readonly(payer, true), // payer can also be the closer
            AccountMeta::new(pda, false),
            AccountMeta::new(payer, false),         // rent recipient
        ],
    );

    let close_accounts = vec![
        (payer, payer_after_open.clone()),
        (pda, pda_after_open.clone()),
    ];

    let close_result = mollusk.process_instruction(&close_ix, &close_accounts);
    assert!(
        matches!(close_result.program_result, ProgramResult::Success),
        "close_digest failed: {:?}",
        close_result.program_result
    );

    let (_, pda_after_close) = close_result
        .resulting_accounts
        .iter()
        .find(|(k, _)| *k == pda)
        .expect("PDA in resulting accounts after close");
    assert_eq!(pda_after_close.lamports, 0, "PDA lamports drained");
    assert_eq!(
        pda_after_close.owner,
        system_program_id(),
        "PDA reassigned to system program"
    );
    assert!(
        pda_after_close.data.is_empty(),
        "PDA data dropped on close, got {} bytes",
        pda_after_close.data.len()
    );

    let (_, payer_after_close) = close_result
        .resulting_accounts
        .iter()
        .find(|(k, _)| *k == payer)
        .expect("payer in resulting accounts after close");
    assert_eq!(
        payer_after_close.lamports,
        payer_after_open.lamports + pda_after_open.lamports,
        "payer refunded full rent"
    );
}

#[test]
fn close_with_wrong_vaa_digest_fails_and_preserves_pda() {
    let mollusk = mollusk();
    let (chain, emitter, sequence, digest, guardian_set_index) = lifecycle_inputs();
    let payer = Pubkey::new_from_array([2u8; 32]);
    let (pda, bump) = derive_digest_pda(chain, &emitter, sequence);

    let open_ix = Instruction::new_with_bytes(
        program_id(),
        &open_digest_ix_data(chain, &emitter, sequence, &digest, guardian_set_index, bump),
        vec![
            AccountMeta::new(payer, true),
            AccountMeta::new(pda, false),
            AccountMeta::new_readonly(system_program_id(), false),
        ],
    );

    let open_accounts = vec![
        (payer, payer_account(10_000_000_000)),
        (pda, uninitialised_pda_account()),
        keyed_account_for_system_program(),
    ];

    let open_result = mollusk.process_instruction(&open_ix, &open_accounts);
    assert!(matches!(open_result.program_result, ProgramResult::Success));

    let pda_after_open = open_result
        .resulting_accounts
        .iter()
        .find(|(k, _)| *k == pda)
        .expect("PDA exists")
        .1
        .clone();
    let payer_after_open = open_result
        .resulting_accounts
        .iter()
        .find(|(k, _)| *k == payer)
        .expect("payer exists")
        .1
        .clone();

    let mut bad_vaa_digest = digest;
    bad_vaa_digest[0] ^= 0xff; // flip a bit so the digests no longer match.

    let close_ix = Instruction::new_with_bytes(
        program_id(),
        &close_digest_ix_data(&bad_vaa_digest),
        vec![
            AccountMeta::new_readonly(payer, true),
            AccountMeta::new(pda, false),
            AccountMeta::new(payer, false),
        ],
    );

    let close_accounts = vec![
        (payer, payer_after_open.clone()),
        (pda, pda_after_open.clone()),
    ];

    let close_result = mollusk.process_instruction(&close_ix, &close_accounts);
    match close_result.program_result {
        ProgramResult::Failure(err) => {
            let code = u64::from(err) as u32;
            assert_eq!(
                code,
                GlobalAccountantError::DigestMismatch as u32,
                "expected DigestMismatch, got {:?}",
                code
            );
        }
        other => panic!("expected Failure(DigestMismatch), got {:?}", other),
    }

    // PDA should still be intact after the failed close.
    let pda_after_close = close_result
        .resulting_accounts
        .iter()
        .find(|(k, _)| *k == pda)
        .expect("PDA still in accounts list")
        .1
        .clone();
    assert_eq!(pda_after_close.owner, program_id());
    assert_eq!(pda_after_close.lamports, pda_after_open.lamports);
    assert_eq!(pda_after_close.data, pda_after_open.data);
}

/// Find a non-canonical (lower) bump that still derives a valid off-curve PDA
/// for the given seeds. Returns the bump and its PDA address.
///
/// `find_program_address` returns the highest bump (counting down from 255) that
/// produces an off-curve point. There are usually additional lower bumps that
/// also produce off-curve points; an attacker who controls the bump in
/// instruction data can use one of those to mint a sibling PDA for the same
/// logical seeds. This helper locates one such sibling for the test.
fn find_non_canonical_bump(
    chain: u16,
    emitter: &[u8; 32],
    sequence: u64,
    canonical_bump: u8,
) -> (u8, Pubkey) {
    let chain_be = chain.to_be_bytes();
    let sequence_be = sequence.to_be_bytes();
    let mut bump = canonical_bump;
    while bump > 0 {
        bump -= 1;
        let seeds: &[&[u8]] = &[DIGEST_SEED_PREFIX, &chain_be, emitter, &sequence_be, &[bump]];
        if let Ok(pda) = Pubkey::create_program_address(seeds, &program_id()) {
            return (bump, pda);
        }
    }
    panic!(
        "no non-canonical bump found for chain={chain} sequence={sequence}; \
         pick different test inputs"
    );
}

#[test]
fn open_with_non_canonical_bump_fails() {
    // Canonical-bump enforcement: an attacker who supplies a lower bump that
    // also produces a valid off-curve PDA must be rejected. Otherwise multiple
    // sibling PDAs can be opened for the same `(chain, emitter, sequence)`,
    // which is the only same-key protection we have until NoReplay lands.
    let mollusk = mollusk();
    let (chain, emitter, sequence, digest, guardian_set_index) = lifecycle_inputs();
    let (_canonical_pda, canonical_bump) = derive_digest_pda(chain, &emitter, sequence);
    let (bad_bump, bad_pda) =
        find_non_canonical_bump(chain, &emitter, sequence, canonical_bump);
    assert_ne!(bad_bump, canonical_bump);

    let payer = Pubkey::new_from_array([9u8; 32]);
    let open_ix = Instruction::new_with_bytes(
        program_id(),
        &open_digest_ix_data(chain, &emitter, sequence, &digest, guardian_set_index, bad_bump),
        vec![
            AccountMeta::new(payer, true),
            AccountMeta::new(bad_pda, false),
            AccountMeta::new_readonly(system_program_id(), false),
        ],
    );

    let open_accounts = vec![
        (payer, payer_account(10_000_000_000)),
        (bad_pda, uninitialised_pda_account()),
        keyed_account_for_system_program(),
    ];

    let result = mollusk.process_instruction(&open_ix, &open_accounts);
    match result.program_result {
        ProgramResult::Failure(err) => {
            let code = u64::from(err) as u32;
            assert_eq!(
                code,
                GlobalAccountantError::InvalidPda as u32,
                "expected InvalidPda for non-canonical bump, got {code:?}"
            );
        }
        other => panic!("expected Failure(InvalidPda), got {:?}", other),
    }
}

#[test]
fn close_with_spoofed_system_owned_pda_fails() {
    // Without an owner check on close, an attacker can fabricate a "pre-account"
    // at the canonical PDA address whose data is a hand-crafted DigestAccount
    // layout naming the attacker as payer, then sweep lamports. The program
    // must reject any close where the PDA is not owned by program_id.
    let mollusk = mollusk();
    let (chain, emitter, sequence, digest, _gsi) = lifecycle_inputs();
    let (pda, _bump) = derive_digest_pda(chain, &emitter, sequence);

    let attacker = Pubkey::new_from_array([0xAAu8; 32]);

    // Hand-craft a DigestAccountLayout with attacker as payer, matching digest.
    // `_padding` is `pub(crate)`, so we go through `Zeroable` + field assignment
    // instead of a struct literal.
    let mut spoof_layout: DigestAccountLayout = bytemuck::Zeroable::zeroed();
    spoof_layout.emitter = emitter;
    spoof_layout.digest = digest;
    spoof_layout.payer = attacker.to_bytes();
    spoof_layout.sequence = sequence;
    spoof_layout.chain = chain;
    let mut spoof_data = vec![0u8; DigestAccountLayout::LEN];
    spoof_data.copy_from_slice(bytemuck::bytes_of(&spoof_layout));

    let spoofed_pda_account = Account {
        lamports: 5_000_000, // some lamports the attacker hopes to drain
        data: spoof_data,
        owner: system_program_id(), // <-- the spoof: owned by system, not program
        executable: false,
        rent_epoch: 0,
    };

    let attacker_starting_lamports = 1_000_000;
    let close_ix = Instruction::new_with_bytes(
        program_id(),
        &close_digest_ix_data(&digest),
        vec![
            AccountMeta::new_readonly(attacker, true),
            AccountMeta::new(pda, false),
            AccountMeta::new(attacker, false),
        ],
    );

    let close_accounts = vec![
        (
            attacker,
            Account {
                lamports: attacker_starting_lamports,
                data: vec![],
                owner: system_program_id(),
                executable: false,
                rent_epoch: 0,
            },
        ),
        (pda, spoofed_pda_account.clone()),
    ];

    let close_result = mollusk.process_instruction(&close_ix, &close_accounts);
    match close_result.program_result {
        ProgramResult::Failure(err) => {
            let code = u64::from(err) as u32;
            assert_eq!(
                code,
                GlobalAccountantError::InvalidPda as u32,
                "expected InvalidPda for system-owned PDA, got {code:?}"
            );
        }
        other => panic!("expected Failure(InvalidPda), got {:?}", other),
    }

    let attacker_after = close_result
        .resulting_accounts
        .iter()
        .find(|(k, _)| *k == attacker)
        .expect("attacker in resulting accounts")
        .1
        .clone();
    assert_eq!(
        attacker_after.lamports, attacker_starting_lamports,
        "no lamports must move to the attacker"
    );
}

#[test]
fn close_with_wrong_rent_recipient_fails() {
    let mollusk = mollusk();
    let (chain, emitter, sequence, digest, guardian_set_index) = lifecycle_inputs();
    let payer = Pubkey::new_from_array([3u8; 32]);
    let (pda, bump) = derive_digest_pda(chain, &emitter, sequence);

    let open_ix = Instruction::new_with_bytes(
        program_id(),
        &open_digest_ix_data(chain, &emitter, sequence, &digest, guardian_set_index, bump),
        vec![
            AccountMeta::new(payer, true),
            AccountMeta::new(pda, false),
            AccountMeta::new_readonly(system_program_id(), false),
        ],
    );

    let open_accounts = vec![
        (payer, payer_account(10_000_000_000)),
        (pda, uninitialised_pda_account()),
        keyed_account_for_system_program(),
    ];

    let open_result = mollusk.process_instruction(&open_ix, &open_accounts);
    assert!(matches!(open_result.program_result, ProgramResult::Success));

    let pda_after_open = open_result
        .resulting_accounts
        .iter()
        .find(|(k, _)| *k == pda)
        .expect("PDA exists")
        .1
        .clone();

    // A different pubkey is supplied as rent recipient.
    let wrong_recipient = Pubkey::new_from_array([0xBBu8; 32]);
    let wrong_recipient_starting = 7_777_777u64;

    let close_ix = Instruction::new_with_bytes(
        program_id(),
        &close_digest_ix_data(&digest),
        vec![
            AccountMeta::new_readonly(payer, true),
            AccountMeta::new(pda, false),
            AccountMeta::new(wrong_recipient, false),
        ],
    );

    let close_accounts = vec![
        (
            payer,
            Account {
                lamports: 0,
                data: vec![],
                owner: system_program_id(),
                executable: false,
                rent_epoch: 0,
            },
        ),
        (pda, pda_after_open.clone()),
        (
            wrong_recipient,
            Account {
                lamports: wrong_recipient_starting,
                data: vec![],
                owner: system_program_id(),
                executable: false,
                rent_epoch: 0,
            },
        ),
    ];

    let close_result = mollusk.process_instruction(&close_ix, &close_accounts);
    match close_result.program_result {
        ProgramResult::Failure(err) => {
            let code = u64::from(err) as u32;
            assert_eq!(
                code,
                GlobalAccountantError::PayerMismatch as u32,
                "expected PayerMismatch for wrong rent recipient, got {code:?}"
            );
        }
        other => panic!("expected Failure(PayerMismatch), got {:?}", other),
    }

    // PDA must still exist with full lamports.
    let pda_after = close_result
        .resulting_accounts
        .iter()
        .find(|(k, _)| *k == pda)
        .expect("PDA in resulting accounts")
        .1
        .clone();
    assert_eq!(pda_after.owner, program_id());
    assert_eq!(pda_after.lamports, pda_after_open.lamports);
    assert_eq!(pda_after.data, pda_after_open.data);

    // Wrong recipient received no lamports.
    let wrong_after = close_result
        .resulting_accounts
        .iter()
        .find(|(k, _)| *k == wrong_recipient)
        .expect("wrong recipient in resulting accounts")
        .1
        .clone();
    assert_eq!(wrong_after.lamports, wrong_recipient_starting);
}

#[test]
fn digest_layout_offsets_pinned() {
    // Belt-and-braces runtime pin against accidental layout drift. The
    // const-asserts in `definitions/src/lib.rs` are the compile-time pin;
    // this is the readable form a reviewer can scan against the design doc.
    use core::mem::offset_of;
    assert_eq!(offset_of!(DigestAccountLayout, emitter), 0);
    assert_eq!(offset_of!(DigestAccountLayout, digest), 32);
    assert_eq!(offset_of!(DigestAccountLayout, payer), 64);
    assert_eq!(offset_of!(DigestAccountLayout, sequence), 96);
    assert_eq!(offset_of!(DigestAccountLayout, quorum_at_slot), 104);
    assert_eq!(offset_of!(DigestAccountLayout, guardian_set_index), 112);
    assert_eq!(offset_of!(DigestAccountLayout, chain), 116);
    assert_eq!(DigestAccountLayout::LEN, 120);
}
