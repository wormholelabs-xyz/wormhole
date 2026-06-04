//! Integration tests for the DigestAccount lifecycle (open + close).
//!
//! Each test drives the on-chain program through mollusk-svm; the program
//! itself must be built via `cargo build-sbf` before `cargo test` runs.
//! The `just test` recipe handles this end-to-end.

use {
    global_accountant_definitions::{
        DigestAccountLayout, GlobalAccountantError, Instruction as IxDiscriminator,
        CORE_BRIDGE_PROGRAM_ID, DIGEST_SEED_PREFIX, VERIFY_VAA_SHIM_PROGRAM_ID,
    },
    mollusk_svm::{program::keyed_account_for_system_program, result::ProgramResult, Mollusk},
    solana_account::Account,
    solana_instruction::{AccountMeta, Instruction},
    solana_pubkey::Pubkey,
};

mod common;
use common::guardian_fixtures::{
    derive_guardian_set_pda, guardian_set_account, guardian_signatures_account, make_guardians,
    sign_digest, GUARDIAN_PUBKEY_LENGTH,
};
use common::mollusk_fixtures::{keyed_account_for_verify_vaa_shim_program, mollusk_with_fixtures};

const PROGRAM_NAME: &str = "global_accountant";
const GUARDIAN_COUNT: usize = 19;
const QUORUM: u8 = 13;

fn program_id() -> Pubkey {
    // Fixed program id so PDA derivation in the test matches the program's
    // on-chain view. The actual bytes are irrelevant for mollusk's purposes.
    Pubkey::new_from_array([7u8; 32])
}

fn mollusk() -> Mollusk {
    mollusk_with_fixtures(&program_id(), PROGRAM_NAME)
}

fn core_bridge_program_id() -> Pubkey {
    Pubkey::new_from_array(CORE_BRIDGE_PROGRAM_ID)
}

fn shim_program_id() -> Pubkey {
    Pubkey::new_from_array(VERIFY_VAA_SHIM_PROGRAM_ID)
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
) -> Vec<u8> {
    // No bump byte travels in the wire: `open_digest_inner` derives the
    // canonical bump on-chain via `find_program_address`.
    let mut data = Vec::with_capacity(1 + 78);
    data.push(IxDiscriminator::TestOnlyOpenDigest as u8);
    data.extend_from_slice(&chain.to_be_bytes());
    data.extend_from_slice(emitter);
    data.extend_from_slice(&sequence.to_be_bytes());
    data.extend_from_slice(digest);
    data.extend_from_slice(&guardian_set_index.to_le_bytes());
    data
}

fn close_digest_ix_data(vaa_digest: &[u8; 32], guardian_set_bump: u8) -> Vec<u8> {
    // 32-byte digest + 1-byte guardian_set_bump. The bump is consumed by the
    // Shim's `VerifyHash` to re-derive the Core Bridge's `GuardianSet` PDA
    // without re-running `find_program_address`.
    let mut data = Vec::with_capacity(1 + 32 + 1);
    data.push(IxDiscriminator::CloseDigest as u8);
    data.extend_from_slice(vaa_digest);
    data.push(guardian_set_bump);
    data
}

/// Three trailing accounts every `close_digest` invocation carries:
/// guardian-signatures PDA, guardian-set PDA, and the Verify VAA Shim
/// program itself. Negative-path tests that fail before reaching the Shim
/// CPI (digest mismatch, payer mismatch, ownership spoof) can pass
/// uninitialised sentinel accounts here; the happy-path test populates
/// them with real fixtures via `close_digest_quorum_extras`.
fn close_digest_extras_sentinel() -> (Vec<AccountMeta>, Vec<(Pubkey, Account)>) {
    let gs_pubkey = Pubkey::new_from_array([0xE1u8; 32]);
    let gset_pubkey = Pubkey::new_from_array([0xE2u8; 32]);
    (
        vec![
            AccountMeta::new_readonly(gs_pubkey, false),
            AccountMeta::new_readonly(gset_pubkey, false),
            AccountMeta::new_readonly(shim_program_id(), false),
        ],
        vec![
            (gs_pubkey, system_owned_account(0)),
            (gset_pubkey, system_owned_account(0)),
            keyed_account_for_verify_vaa_shim_program(),
        ],
    )
}

/// Build the three trailing Shim accounts with real GuardianSignatures +
/// GuardianSet fixtures so the Shim's `VerifyHash` CPI succeeds. Returns the
/// guardian_set_bump alongside the metas/accounts pair so the caller can pin
/// the close_digest ix data to the canonical Shim derivation.
fn close_digest_extras_quorum(
    digest: &[u8; 32],
    guardian_set_index: u32,
    refund_recipient: &Pubkey,
) -> (Vec<AccountMeta>, Vec<(Pubkey, Account)>, u8) {
    let guardians = make_guardians(GUARDIAN_COUNT, 0x42);
    let keys: Vec<[u8; GUARDIAN_PUBKEY_LENGTH]> = guardians.iter().map(|g| g.eth_address).collect();
    let (guardian_set_pubkey, guardian_set_bump) =
        derive_guardian_set_pda(guardian_set_index, &core_bridge_program_id());
    let gs_pubkey = Pubkey::new_from_array([0xE1u8; 32]);
    let sigs: Vec<(u8, [u8; 65])> = (0..QUORUM)
        .map(|i| (i, sign_digest(&guardians[i as usize], digest)))
        .collect();
    (
        vec![
            AccountMeta::new_readonly(gs_pubkey, false),
            AccountMeta::new_readonly(guardian_set_pubkey, false),
            AccountMeta::new_readonly(shim_program_id(), false),
        ],
        vec![
            (
                gs_pubkey,
                guardian_signatures_account(
                    guardian_set_index,
                    refund_recipient,
                    &sigs,
                    &shim_program_id(),
                ),
            ),
            (
                guardian_set_pubkey,
                guardian_set_account(guardian_set_index, &keys, 0, 0, &core_bridge_program_id()),
            ),
            keyed_account_for_verify_vaa_shim_program(),
        ],
        guardian_set_bump,
    )
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

/// Build a system-owned account with the given lamports and zero data. Used
/// both for the regular payer and for dust-prefunded PDAs.
fn system_owned_account(lamports: u64) -> Account {
    Account {
        lamports,
        data: vec![],
        owner: system_program_id(),
        executable: false,
        rent_epoch: 0,
    }
}

fn payer_account(lamports: u64) -> Account {
    system_owned_account(lamports)
}

fn uninitialised_pda_account() -> Account {
    system_owned_account(0)
}

/// Post-open state. Used by every negative test that starts from an
/// already-opened PDA so each test body begins from a single line.
struct OpenState {
    pda: Pubkey,
    pda_after_open: Account,
    payer_after_open: Account,
    payer_starting_lamports: u64,
}

/// Drive `open_digest` end-to-end and return the post-open state. Asserts
/// success internally so callers can focus on the negative path under test.
///
/// `pda_initial_lamports` lets the dust-DoS test pre-fund the PDA at the
/// canonical address with non-zero lamports while keeping it system-owned and
/// data-empty (mimicking the `system_program::transfer` grief attack).
fn open_lifecycle_setup(
    mollusk: &Mollusk,
    payer: Pubkey,
    payer_starting_lamports: u64,
    pda_initial_lamports: u64,
) -> OpenState {
    let (chain, emitter, sequence, digest, guardian_set_index) = lifecycle_inputs();
    let (pda, _) = derive_digest_pda(chain, &emitter, sequence);

    let open_ix = Instruction::new_with_bytes(
        program_id(),
        &open_digest_ix_data(chain, &emitter, sequence, &digest, guardian_set_index),
        vec![
            AccountMeta::new(payer, true),
            AccountMeta::new(pda, false),
            AccountMeta::new_readonly(system_program_id(), false),
        ],
    );

    let pda_account = if pda_initial_lamports == 0 {
        uninitialised_pda_account()
    } else {
        system_owned_account(pda_initial_lamports)
    };

    let open_accounts = vec![
        (payer, payer_account(payer_starting_lamports)),
        (pda, pda_account),
        keyed_account_for_system_program(),
    ];

    let open_result = mollusk.process_instruction(&open_ix, &open_accounts);
    assert!(
        matches!(open_result.program_result, ProgramResult::Success),
        "open_digest failed: {:?}",
        open_result.program_result
    );

    let pda_after_open = open_result
        .resulting_accounts
        .iter()
        .find(|(k, _)| *k == pda)
        .expect("PDA in resulting accounts")
        .1
        .clone();
    let payer_after_open = open_result
        .resulting_accounts
        .iter()
        .find(|(k, _)| *k == payer)
        .expect("payer in resulting accounts")
        .1
        .clone();

    OpenState {
        pda,
        pda_after_open,
        payer_after_open,
        payer_starting_lamports,
    }
}

#[test]
fn open_then_close_round_trip() {
    let mollusk = mollusk();
    let (_chain, _emitter, _sequence, digest, guardian_set_index) = lifecycle_inputs();
    let payer = Pubkey::new_from_array([1u8; 32]);

    let state = open_lifecycle_setup(&mollusk, payer, 10_000_000_000, 0);

    // -- Open assertions ----------------------------------------------------
    assert_eq!(state.pda_after_open.owner, program_id(), "PDA owner");
    assert_eq!(
        state.pda_after_open.data.len(),
        DigestAccountLayout::LEN,
        "PDA data length"
    );

    let stored: &DigestAccountLayout = bytemuck::from_bytes(&state.pda_after_open.data);
    let (chain, emitter, sequence, _digest, _gsi) = lifecycle_inputs();
    assert_eq!(stored.chain, chain);
    assert_eq!(stored.emitter, emitter);
    assert_eq!(stored.sequence, sequence);
    assert_eq!(stored.digest, digest);
    assert_eq!(stored.payer, payer.to_bytes());
    assert_eq!(stored.guardian_set_index, guardian_set_index);
    // Slot is set by the runtime; just assert it was written.
    assert_ne!(stored.quorum_at_slot, u64::MAX);

    let payer_paid = state
        .payer_starting_lamports
        .saturating_sub(state.payer_after_open.lamports);
    assert!(payer_paid > 0, "payer should have funded rent");
    assert_eq!(
        payer_paid, state.pda_after_open.lamports,
        "rent debit must equal PDA balance"
    );

    // -- Close --------------------------------------------------------------
    // Happy-path: build real GuardianSignatures + GuardianSet fixtures so the
    // Shim's VerifyHash CPI succeeds. The refund recipient encoded in the
    // GuardianSignatures account is the payer — irrelevant for close_digest
    // itself, but the Shim requires the field to be present.
    let (extra_metas, extra_accounts, guardian_set_bump) =
        close_digest_extras_quorum(&digest, guardian_set_index, &payer);
    let mut metas = vec![
        AccountMeta::new_readonly(payer, true), // payer can also be the closer
        AccountMeta::new(state.pda, false),
        AccountMeta::new(payer, false), // rent recipient
    ];
    metas.extend(extra_metas);
    let close_ix = Instruction::new_with_bytes(
        program_id(),
        &close_digest_ix_data(&digest, guardian_set_bump),
        metas,
    );

    let mut close_accounts = vec![
        (payer, state.payer_after_open.clone()),
        (state.pda, state.pda_after_open.clone()),
    ];
    close_accounts.extend(extra_accounts);

    let close_result = mollusk.process_instruction(&close_ix, &close_accounts);
    assert!(
        matches!(close_result.program_result, ProgramResult::Success),
        "close_digest failed: {:?}",
        close_result.program_result
    );

    let (_, pda_after_close) = close_result
        .resulting_accounts
        .iter()
        .find(|(k, _)| *k == state.pda)
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
        state.payer_after_open.lamports + state.pda_after_open.lamports,
        "payer refunded full rent"
    );
}

#[test]
fn open_digest_with_prefunded_pda_succeeds() {
    // Pre-funded PDA acceptance: anyone can `system_program::transfer(N)` to
    // the canonical PDA address before the legitimate open. A naive
    // `CreateAccount` CPI would fail ("account already in use") and DoS the
    // (chain, emitter, sequence). The open path falls back to Transfer top-up
    // + Allocate + Assign when the PDA already holds lamports but is
    // otherwise system-owned and data-empty.
    //
    // Two cases pin the branch endpoints:
    //   * dust (1 lamport) -> payer tops up to rent-exempt minimum.
    //   * overshoot (1 SOL, well above the ~0.0009 SOL minimum for a
    //     120-byte account) -> Transfer is skipped (saturating_sub == 0); the
    //     pre-funded balance is accepted as a gift.
    let cases: [(&str, u64, u8); 2] = [
        ("dust", 1, 0xC1),
        ("overshoot (1 SOL)", 1_000_000_000, 0xD1),
    ];

    for (label, prefunded, payer_seed) in cases {
        let mollusk = mollusk();
        let (_chain, _emitter, _sequence, digest, _gsi) = lifecycle_inputs();
        let payer = Pubkey::new_from_array([payer_seed; 32]);
        let payer_starting = 10_000_000_000_u64;

        let state = open_lifecycle_setup(&mollusk, payer, payer_starting, prefunded);

        assert_eq!(
            state.pda_after_open.owner,
            program_id(),
            "[{label}] PDA owner"
        );
        assert_eq!(
            state.pda_after_open.data.len(),
            DigestAccountLayout::LEN,
            "[{label}] PDA allocated to full layout length"
        );

        let stored: &DigestAccountLayout = bytemuck::from_bytes(&state.pda_after_open.data);
        assert_eq!(stored.payer, payer.to_bytes(), "[{label}] payer recorded");
        assert_eq!(stored.digest, digest, "[{label}] digest stored");

        // Universal accounting invariant: payer paid the delta between the
        // pre-funded amount and the resulting PDA balance (saturating to 0).
        let payer_paid = state
            .payer_starting_lamports
            .saturating_sub(state.payer_after_open.lamports);
        let expected_paid = state.pda_after_open.lamports.saturating_sub(prefunded);
        assert_eq!(
            payer_paid, expected_paid,
            "[{label}] payer paid delta between pre-funded and final PDA balance"
        );
        assert!(
            state.pda_after_open.lamports >= prefunded,
            "[{label}] PDA balance never drops below the pre-funded amount"
        );
    }
}

#[test]
fn close_with_wrong_vaa_digest_fails_and_preserves_pda() {
    let mollusk = mollusk();
    let (_chain, _emitter, _sequence, digest, _gsi) = lifecycle_inputs();
    let payer = Pubkey::new_from_array([2u8; 32]);

    let state = open_lifecycle_setup(&mollusk, payer, 10_000_000_000, 0);

    let mut bad_vaa_digest = digest;
    bad_vaa_digest[0] ^= 0xff; // flip a bit so the digests no longer match.

    let (extra_metas, extra_accounts) = close_digest_extras_sentinel();
    let mut metas = vec![
        AccountMeta::new_readonly(payer, true),
        AccountMeta::new(state.pda, false),
        AccountMeta::new(payer, false),
    ];
    metas.extend(extra_metas);
    let close_ix = Instruction::new_with_bytes(
        program_id(),
        &close_digest_ix_data(&bad_vaa_digest, 0),
        metas,
    );

    let mut close_accounts = vec![
        (payer, state.payer_after_open.clone()),
        (state.pda, state.pda_after_open.clone()),
    ];
    close_accounts.extend(extra_accounts);

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
        .find(|(k, _)| *k == state.pda)
        .expect("PDA still in accounts list")
        .1
        .clone();
    assert_eq!(pda_after_close.owner, program_id());
    assert_eq!(pda_after_close.lamports, state.pda_after_open.lamports);
    assert_eq!(pda_after_close.data, state.pda_after_open.data);
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
    let (extra_metas, extra_accounts) = close_digest_extras_sentinel();
    let mut metas = vec![
        AccountMeta::new_readonly(attacker, true),
        AccountMeta::new(pda, false),
        AccountMeta::new(attacker, false),
    ];
    metas.extend(extra_metas);
    let close_ix =
        Instruction::new_with_bytes(program_id(), &close_digest_ix_data(&digest, 0), metas);

    let mut close_accounts = vec![
        (attacker, system_owned_account(attacker_starting_lamports)),
        (pda, spoofed_pda_account.clone()),
    ];
    close_accounts.extend(extra_accounts);

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
    let (_chain, _emitter, _sequence, digest, _gsi) = lifecycle_inputs();
    let payer = Pubkey::new_from_array([3u8; 32]);

    let state = open_lifecycle_setup(&mollusk, payer, 10_000_000_000, 0);

    // A different pubkey is supplied as rent recipient.
    let wrong_recipient = Pubkey::new_from_array([0xBBu8; 32]);
    let wrong_recipient_starting = 7_777_777u64;

    let (extra_metas, extra_accounts) = close_digest_extras_sentinel();
    let mut metas = vec![
        AccountMeta::new_readonly(payer, true),
        AccountMeta::new(state.pda, false),
        AccountMeta::new(wrong_recipient, false),
    ];
    metas.extend(extra_metas);
    let close_ix =
        Instruction::new_with_bytes(program_id(), &close_digest_ix_data(&digest, 0), metas);

    let mut close_accounts = vec![
        (payer, system_owned_account(0)),
        (state.pda, state.pda_after_open.clone()),
        (
            wrong_recipient,
            system_owned_account(wrong_recipient_starting),
        ),
    ];
    close_accounts.extend(extra_accounts);

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
        .find(|(k, _)| *k == state.pda)
        .expect("PDA in resulting accounts")
        .1
        .clone();
    assert_eq!(pda_after.owner, program_id());
    assert_eq!(pda_after.lamports, state.pda_after_open.lamports);
    assert_eq!(pda_after.data, state.pda_after_open.data);

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
    // this is the human-readable form.
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

/// Production builds gate `test_only_open_digest` behind the `test-only-open-digest`
/// Cargo feature. The dispatch site short-circuits the
/// `Instruction::TestOnlyOpenDigest` arm to `NotEnabled` when the feature is
/// off; the on-chain `.so` shipped to mainnet must be built with the feature
/// off, while the `.so` mollusk loads for these tests must be built with it on.
///
/// We pin both sides by reading a `pub const` exported from the program crate
/// that mirrors `cfg!(feature = "test-only-open-digest")` from inside the
/// program. A negative test ("TestOnlyOpenDigest discriminator returns NotEnabled in a
/// prod build") would need a second `.so` and lives in the `just build-prod`
/// pipeline instead — that target additionally fails on `close_digest`'s
/// mock-vaa `compile_error!`, so a successful prod build is unreachable until
/// the real VAA Shim CPI lands.
#[test]
fn open_digest_gated_on_for_test_build() {
    // `TEST_ONLY_OPEN_DIGEST_ENABLED` is `pub const bool` exported by the
    // program crate, so this assertion resolves at compile time — that's the
    // point. Clippy's `assertions-on-constants` lint flags `assert!(true)`,
    // so wrap the check in a conditional panic to preserve the test
    // semantics (the build will fail if the const is false).
    if !global_accountant::TEST_ONLY_OPEN_DIGEST_ENABLED {
        panic!(
            "test build must enable test-only-open-digest; \
             see programs/global-accountant/Cargo.toml dev-dependencies"
        );
    }
}
