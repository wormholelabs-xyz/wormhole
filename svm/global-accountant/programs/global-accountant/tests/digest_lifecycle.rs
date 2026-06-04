//! DigestAccount lifecycle (open + close) tests, driven through mollusk-svm.
//! Requires `cargo build-sbf` first; `just test` handles this.

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
    // Fixed so test-side PDA derivation matches the program's view.
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
    // No bump on the wire; the program derives it on-chain.
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
    // 32-byte digest + 1-byte guardian_set_bump (the Shim re-derives the
    // GuardianSet PDA from the bump).
    let mut data = Vec::with_capacity(1 + 32 + 1);
    data.push(IxDiscriminator::CloseDigest as u8);
    data.extend_from_slice(vaa_digest);
    data.push(guardian_set_bump);
    data
}

/// Sentinel versions of the three trailing close_digest accounts
/// (guardian-signatures PDA, guardian-set PDA, Shim program). For negative
/// tests that fail before the Shim CPI; the happy path uses
/// `close_digest_extras_quorum` instead.
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

/// Real GuardianSignatures + GuardianSet fixtures for the three trailing close
/// accounts so the Shim's `VerifyHash` CPI succeeds. Returns the
/// guardian_set_bump for the close_digest ix data.
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

/// System-owned account with the given lamports and zero data.
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

/// Post-open state shared by negative tests that start from an opened PDA.
struct OpenState {
    pda: Pubkey,
    pda_after_open: Account,
    payer_after_open: Account,
    payer_starting_lamports: u64,
}

/// Drive `open_digest` and return the post-open state, asserting success
/// internally. `pda_initial_lamports > 0` pre-funds the PDA (system-owned,
/// data-empty) to exercise the prefunded-PDA path.
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

/// open_digest then close_digest succeed, refunding rent to the payer.
#[test]
fn open_then_close_round_trip() {
    let mollusk = mollusk();
    let (_chain, _emitter, _sequence, digest, guardian_set_index) = lifecycle_inputs();
    let payer = Pubkey::new_from_array([1u8; 32]);

    let state = open_lifecycle_setup(&mollusk, payer, 10_000_000_000, 0);

    // Open assertions.
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

    // Close with real Shim fixtures so VerifyHash succeeds.
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

/// open_digest accepts a PDA pre-funded via `system_program::transfer` (a DoS
/// vector against naive CreateAccount), topping up dust and accepting overshoot.
#[test]
fn open_digest_with_prefunded_pda_succeeds() {
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

        // Payer paid the delta between pre-funded and final PDA balance.
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

/// close_digest with a digest that differs from the stored one fails with
/// DigestMismatch and leaves the PDA intact.
#[test]
fn close_with_wrong_vaa_digest_fails_and_preserves_pda() {
    let mollusk = mollusk();
    let (_chain, _emitter, _sequence, digest, _gsi) = lifecycle_inputs();
    let payer = Pubkey::new_from_array([2u8; 32]);

    let state = open_lifecycle_setup(&mollusk, payer, 10_000_000_000, 0);

    let mut bad_vaa_digest = digest;
    bad_vaa_digest[0] ^= 0xff;

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

    // PDA intact after the failed close.
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

/// close_digest rejects a system-owned PDA carrying a hand-crafted layout that
/// names the attacker as payer (InvalidPda), so no lamports move.
#[test]
fn close_with_spoofed_system_owned_pda_fails() {
    let mollusk = mollusk();
    let (chain, emitter, sequence, digest, _gsi) = lifecycle_inputs();
    let (pda, _bump) = derive_digest_pda(chain, &emitter, sequence);

    let attacker = Pubkey::new_from_array([0xAAu8; 32]);

    // `_padding` is `pub(crate)`, so build via Zeroable + field assignment.
    let mut spoof_layout: DigestAccountLayout = bytemuck::Zeroable::zeroed();
    spoof_layout.emitter = emitter;
    spoof_layout.digest = digest;
    spoof_layout.payer = attacker.to_bytes();
    spoof_layout.sequence = sequence;
    spoof_layout.chain = chain;
    let mut spoof_data = vec![0u8; DigestAccountLayout::LEN];
    spoof_data.copy_from_slice(bytemuck::bytes_of(&spoof_layout));

    let spoofed_pda_account = Account {
        lamports: 5_000_000,
        data: spoof_data,
        owner: system_program_id(), // the spoof: system-owned, not program-owned
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

/// close_digest with a rent recipient other than the recorded payer fails with
/// PayerMismatch; no lamports move.
#[test]
fn close_with_wrong_rent_recipient_fails() {
    let mollusk = mollusk();
    let (_chain, _emitter, _sequence, digest, _gsi) = lifecycle_inputs();
    let payer = Pubkey::new_from_array([3u8; 32]);

    let state = open_lifecycle_setup(&mollusk, payer, 10_000_000_000, 0);

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

/// Runtime pin of DigestAccountLayout field offsets against drift (the
/// compile-time pin lives in `definitions/src/lib.rs`).
#[test]
fn digest_layout_offsets_pinned() {
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

/// This test binary must be built with `test-only-open-digest` on (the mainnet
/// `.so` ships with it off). Pinned via the `TEST_ONLY_OPEN_DIGEST_ENABLED`
/// const the program exports.
#[test]
fn open_digest_gated_on_for_test_build() {
    // Conditional panic rather than `assert!` to dodge clippy's
    // `assertions-on-constants` lint on the compile-time const.
    if !global_accountant::TEST_ONLY_OPEN_DIGEST_ENABLED {
        panic!(
            "test build must enable test-only-open-digest; \
             see programs/global-accountant/Cargo.toml dev-dependencies"
        );
    }
}
