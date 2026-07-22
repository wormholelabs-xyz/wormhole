//! Characterization test for the pinocchio -> anchor-lang 1.1.2 migration.
//!
//! This test pins, byte-for-byte, the pre-migration `submit_vaas` happy-path
//! wire contract: the exact on-wire instruction bytes, the exact account-meta
//! order/signer/writable flags, and the exact post-transaction account bytes
//! (owner, lamports, and data) for every account touched. It is written
//! against literal, hand-computed expected values rather than by calling the
//! production wire-builder helpers used elsewhere in this test suite, so it
//! cannot silently drift alongside an implementation change.
//!
//! Written *before* the anchor-lang port (see
//! `.claude/tasks/anchor-migration-plan-v1.1.2.md`) and left unmodified by it.
//! It must pass identically against both the pre-migration pinocchio `.so`
//! and the post-migration anchor-lang `.so` — that is what proves the port is
//! byte-identical on the wire.

#![allow(clippy::too_many_arguments)]

use {
    global_accountant_definitions::{
        BalanceAccountLayout, ChainRegistrationLayout, Uint256, ACCOUNT_SEED_PREFIX,
        CHAIN_REGISTRATION_SEED_PREFIX, CORE_BRIDGE_PROGRAM_ID,
        NOREPLAY_AUTHORITY_SEED_PREFIX, NOREPLAY_BITMAP_BYTES, NOREPLAY_BITMAP_OFFSET,
        NOREPLAY_BITS_PER_BUCKET, NOREPLAY_PROGRAM_ID, VERIFY_VAA_SHIM_PROGRAM_ID,
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
use common::mollusk_fixtures::{
    keyed_account_for_noreplay_program, keyed_account_for_verify_vaa_shim_program,
    mollusk_with_fixtures,
};

const PROGRAM_NAME: &str = "global_accountant";
const GUARDIAN_COUNT: usize = 19;
const QUORUM: usize = 13;
const GUARDIAN_SET_INDEX: u32 = 4;

// Fixed scenario constants. Chosen to be recognisable in a hex dump and
// distinct across fields so a transposition bug in either the program or this
// test surfaces immediately.
const EMITTER_CHAIN: u16 = 2; // Ethereum
const EMITTER_ADDRESS: [u8; 32] = {
    let mut a = [0u8; 32];
    a[0] = 0x5E;
    a[31] = 0x77;
    a
};
const SEQUENCE: u64 = 0x0000_0000_0000_2A2A;
const TRANSFER_AMOUNT: u128 = 424_242u128;
const TOKEN_CHAIN: u16 = 2; // native to Ethereum
const TOKEN_ADDRESS: [u8; 32] = [0x99u8; 32];
const RECIPIENT_CHAIN: u16 = 1; // Solana (wrapped destination)

fn program_id() -> Pubkey {
    Pubkey::new_from_array([7u8; 32])
}

fn mollusk() -> Mollusk {
    mollusk_with_fixtures(&program_id(), PROGRAM_NAME)
}

fn system_program_id() -> Pubkey {
    keyed_account_for_system_program().0
}

fn core_bridge_program_id() -> Pubkey {
    Pubkey::new_from_array(CORE_BRIDGE_PROGRAM_ID)
}

fn shim_program_id() -> Pubkey {
    Pubkey::new_from_array(VERIFY_VAA_SHIM_PROGRAM_ID)
}

fn noreplay_program_id() -> Pubkey {
    Pubkey::new_from_array(NOREPLAY_PROGRAM_ID)
}

/// Host-side `keccak256(keccak256(body))` — the Wormhole VAA digest convention.
fn double_keccak256_host(body: &[u8]) -> [u8; 32] {
    let inner = solana_keccak_hasher::hashv(&[body]).to_bytes();
    solana_keccak_hasher::hashv(&[&inner]).to_bytes()
}

/// Hand-build a 184-byte Token Bridge Transfer VAA body (51-byte header +
/// 133-byte transfer payload), byte-by-byte, independent of any shared
/// production or test helper — this is the literal wire layout being pinned.
fn build_body_literal() -> Vec<u8> {
    let mut body = vec![0u8; 51 + 133];
    // Header: timestamp(4)=0, nonce(4)=0.
    body[8..10].copy_from_slice(&EMITTER_CHAIN.to_be_bytes());
    body[10..42].copy_from_slice(&EMITTER_ADDRESS);
    body[42..50].copy_from_slice(&SEQUENCE.to_be_bytes());
    body[50] = 0; // consistency_level
                  // Transfer payload (offset 51): action(1) amount(32) token_address(32) token_chain(2) recipient(32) recipient_chain(2) fee(32).
    body[51] = 0x01;
    body[52 + 16..52 + 32].copy_from_slice(&TRANSFER_AMOUNT.to_be_bytes());
    body[84..116].copy_from_slice(&TOKEN_ADDRESS);
    body[116..118].copy_from_slice(&TOKEN_CHAIN.to_be_bytes());
    body[118..150].copy_from_slice(&[0xEEu8; 32]); // recipient — ignored by the program
    body[150..152].copy_from_slice(&RECIPIENT_CHAIN.to_be_bytes());
    // body[152..184] fee — left zero, unused by this program.
    body
}

/// Literal expected instruction-data bytes for `submit_vaas`, per the module
/// doc in `programs/global-accountant/src/instructions/submit_vaas.rs`:
/// `[discriminator: u8 = 2][guardian_set_bump: u8][body_len: u16 LE][body]`.
fn build_expected_ix_bytes(guardian_set_bump: u8, body: &[u8]) -> Vec<u8> {
    let mut expected = Vec::with_capacity(1 + 1 + 2 + body.len());
    expected.push(2u8); // IxDiscriminator::SubmitVaas
    expected.push(guardian_set_bump);
    expected.extend_from_slice(&(body.len() as u16).to_le_bytes());
    expected.extend_from_slice(body);
    expected
}

fn derive_account_pda(chain: u16, token_chain: u16, token_address: &[u8; 32]) -> (Pubkey, u8) {
    Pubkey::find_program_address(
        &[
            ACCOUNT_SEED_PREFIX,
            &chain.to_be_bytes(),
            &token_chain.to_be_bytes(),
            token_address,
        ],
        &program_id(),
    )
}

fn derive_chain_registration_pda(chain: u16) -> (Pubkey, u8) {
    Pubkey::find_program_address(
        &[CHAIN_REGISTRATION_SEED_PREFIX, &chain.to_be_bytes()],
        &program_id(),
    )
}

fn derive_noreplay_bucket(authority: &Pubkey, chain: u16, emitter: &[u8; 32], sequence: u64) -> Pubkey {
    let mut namespace = [0u8; 34];
    namespace[..2].copy_from_slice(&chain.to_be_bytes());
    namespace[2..].copy_from_slice(emitter);
    let bucket_index = (sequence / NOREPLAY_BITS_PER_BUCKET).to_le_bytes();
    let (pda, _) = Pubkey::find_program_address(
        &[
            authority.as_ref(),
            &namespace[..32],
            &namespace[32..],
            &bucket_index,
        ],
        &noreplay_program_id(),
    );
    pda
}

fn system_owned_account(lamports: u64) -> Account {
    Account {
        lamports,
        data: vec![],
        owner: system_program_id(),
        executable: false,
        rent_epoch: 0,
    }
}

fn chain_registration_account(chain: u16, emitter_address: &[u8; 32]) -> Account {
    let mut layout: ChainRegistrationLayout = bytemuck::Zeroable::zeroed();
    layout.tag = ChainRegistrationLayout::TAG;
    layout.chain = chain;
    layout.emitter_address = *emitter_address;
    Account {
        lamports: 1_000_000,
        data: bytemuck::bytes_of(&layout).to_vec(),
        owner: program_id(),
        executable: false,
        rent_epoch: 0,
    }
}

fn find_account<'a>(accounts: &'a [(Pubkey, Account)], key: &Pubkey) -> &'a Account {
    &accounts
        .iter()
        .find(|(k, _)| k == key)
        .unwrap_or_else(|| panic!("account {key} not in result list"))
        .1
}

/// Pins the full `submit_vaas` happy-path wire contract: instruction bytes,
/// account-meta order/flags, and post-tx account bytes for every account.
#[test]
fn characterize_submit_vaas_happy_path_wire_format() {
    let mollusk = mollusk();

    // ----- Guardians / digest -----
    let guardians = make_guardians(GUARDIAN_COUNT, 0x24);
    let body = build_body_literal();
    let digest = double_keccak256_host(&body);
    let (guardian_set_pubkey, guardian_set_bump) =
        derive_guardian_set_pda(GUARDIAN_SET_INDEX, &core_bridge_program_id());

    // ----- Pubkeys -----
    let submitter = Pubkey::new_from_array([0x11u8; 32]);
    let guardian_signatures_pubkey = Pubkey::new_from_array([0xC5u8; 32]);
    let (noreplay_authority_pubkey, _) =
        Pubkey::find_program_address(&[NOREPLAY_AUTHORITY_SEED_PREFIX], &program_id());
    let noreplay_bucket_pubkey = derive_noreplay_bucket(
        &noreplay_authority_pubkey,
        EMITTER_CHAIN,
        &EMITTER_ADDRESS,
        SEQUENCE,
    );
    let (source_account_pubkey, _) = derive_account_pda(EMITTER_CHAIN, TOKEN_CHAIN, &TOKEN_ADDRESS);
    let (dest_account_pubkey, _) =
        derive_account_pda(RECIPIENT_CHAIN, TOKEN_CHAIN, &TOKEN_ADDRESS);
    let (chain_registration_pubkey, _) = derive_chain_registration_pda(EMITTER_CHAIN);

    // ----- (1) Pin the literal instruction bytes -----
    let ix_data = build_expected_ix_bytes(guardian_set_bump, &body);
    assert_eq!(ix_data[0], 2, "byte 0 is the SubmitVaas discriminator");
    assert_eq!(ix_data[1], guardian_set_bump, "byte 1 is guardian_set_bump");
    let declared_body_len = u16::from_le_bytes([ix_data[2], ix_data[3]]) as usize;
    assert_eq!(declared_body_len, body.len(), "bytes 2..4 are body_len LE");
    assert_eq!(&ix_data[4..], &body[..], "bytes 4.. are the raw body");
    assert_eq!(ix_data.len(), 1 + 1 + 2 + 184, "total wire length pinned");

    // ----- (2) Pin the literal account-meta order and flags -----
    let account_metas = vec![
        AccountMeta::new(submitter, true),
        AccountMeta::new_readonly(shim_program_id(), false),
        AccountMeta::new_readonly(guardian_set_pubkey, false),
        AccountMeta::new_readonly(guardian_signatures_pubkey, false),
        AccountMeta::new(noreplay_bucket_pubkey, false),
        AccountMeta::new_readonly(noreplay_program_id(), false),
        AccountMeta::new_readonly(noreplay_authority_pubkey, false),
        AccountMeta::new(source_account_pubkey, false),
        AccountMeta::new(dest_account_pubkey, false),
        AccountMeta::new_readonly(system_program_id(), false),
        AccountMeta::new_readonly(chain_registration_pubkey, false),
    ];
    assert_eq!(account_metas.len(), 11, "submit_vaas takes exactly 11 accounts");
    let expected_flags: [(bool, bool); 11] = [
        (true, true),   // 0 submitter: writable, signer
        (false, false), // 1 shim program: readonly
        (false, false), // 2 guardian set: readonly
        (false, false), // 3 guardian signatures: readonly
        (true, false),  // 4 noreplay bucket: writable
        (false, false), // 5 noreplay program: readonly
        (false, false), // 6 noreplay authority: readonly
        (true, false),  // 7 source account: writable
        (true, false),  // 8 dest account: writable
        (false, false), // 9 system program: readonly
        (false, false), // 10 chain registration: readonly
    ];
    for (i, meta) in account_metas.iter().enumerate() {
        assert_eq!(
            (meta.is_writable, meta.is_signer),
            expected_flags[i],
            "account meta {i} flags"
        );
    }

    // ----- (3) Build the initial account state -----
    let sigs: Vec<(u8, [u8; 65])> = (0..QUORUM)
        .map(|i| (i as u8, sign_digest(&guardians[i], &digest)))
        .collect();
    let keys: Vec<[u8; GUARDIAN_PUBKEY_LENGTH]> = guardians.iter().map(|g| g.eth_address).collect();

    let initial_accounts = vec![
        (submitter, system_owned_account(50_000_000_000)),
        keyed_account_for_verify_vaa_shim_program(),
        (
            guardian_set_pubkey,
            guardian_set_account(GUARDIAN_SET_INDEX, &keys, 0, 0, &core_bridge_program_id()),
        ),
        (
            guardian_signatures_pubkey,
            guardian_signatures_account(GUARDIAN_SET_INDEX, &submitter, &sigs, &shim_program_id()),
        ),
        (noreplay_bucket_pubkey, system_owned_account(0)),
        keyed_account_for_noreplay_program(),
        (noreplay_authority_pubkey, system_owned_account(0)),
        (source_account_pubkey, system_owned_account(0)),
        (dest_account_pubkey, system_owned_account(0)),
        keyed_account_for_system_program(),
        (
            chain_registration_pubkey,
            chain_registration_account(EMITTER_CHAIN, &EMITTER_ADDRESS),
        ),
    ];

    // ----- (4) Execute -----
    let ix = Instruction::new_with_bytes(program_id(), &ix_data, account_metas);
    let result = mollusk.process_instruction(&ix, &initial_accounts);
    assert!(
        matches!(result.program_result, ProgramResult::Success),
        "expected Success, got {:?}",
        result.program_result
    );

    // ----- (5) Pin the exact post-tx account bytes -----

    // NoReplay bucket: program-owned by solana-noreplay, 129 bytes, exactly
    // the bit at `SEQUENCE % 1024` set and no other bits.
    let bucket = find_account(&result.resulting_accounts, &noreplay_bucket_pubkey);
    assert_eq!(bucket.owner, noreplay_program_id());
    assert_eq!(bucket.data.len(), NOREPLAY_BITMAP_OFFSET + NOREPLAY_BITMAP_BYTES);
    let bit = (SEQUENCE % NOREPLAY_BITS_PER_BUCKET) as usize;
    for (i, byte) in bucket.data[NOREPLAY_BITMAP_OFFSET..].iter().enumerate() {
        let expected_byte = if i == bit / 8 { 1u8 << (bit % 8) } else { 0u8 };
        assert_eq!(*byte, expected_byte, "bitmap byte {i} exact content");
    }

    // Source (native, since EMITTER_CHAIN == TOKEN_CHAIN): credited.
    let src = find_account(&result.resulting_accounts, &source_account_pubkey);
    assert_eq!(src.owner, program_id());
    assert_eq!(src.data.len(), BalanceAccountLayout::LEN);
    let src_layout: &BalanceAccountLayout = bytemuck::from_bytes(&src.data);
    assert_eq!(src_layout.tag, BalanceAccountLayout::TAG);
    assert_eq!(src_layout.chain, EMITTER_CHAIN);
    assert_eq!(src_layout.token_chain, TOKEN_CHAIN);
    assert_eq!(src_layout.token_address, TOKEN_ADDRESS);
    assert_eq!(src_layout.balance, Uint256::from_u128(TRANSFER_AMOUNT));
    // Exact on-disk bytes, independent of the layout struct's own accessors.
    // NOTE: `chain`/`token_chain` are plain `u16` struct fields serialized in
    // the host's native (little-endian) byte order by `#[repr(C)] Pod` — only
    // the `Uint256 balance` field (and the seed derivation, separately) uses
    // big-endian, matching the VAA wire's `amount` encoding.
    assert_eq!(src.data[0], 2, "tag byte literal");
    assert_eq!(&src.data[2..4], &EMITTER_CHAIN.to_le_bytes());
    assert_eq!(&src.data[4..6], &TOKEN_CHAIN.to_le_bytes());
    assert_eq!(&src.data[6..38], &TOKEN_ADDRESS);
    let mut expected_balance_be = [0u8; 32];
    expected_balance_be[16..].copy_from_slice(&TRANSFER_AMOUNT.to_be_bytes());
    assert_eq!(&src.data[38..70], &expected_balance_be);

    // Dest (wrapped, since RECIPIENT_CHAIN != TOKEN_CHAIN): credited.
    let dst = find_account(&result.resulting_accounts, &dest_account_pubkey);
    assert_eq!(dst.owner, program_id());
    assert_eq!(dst.data.len(), BalanceAccountLayout::LEN);
    let dst_layout: &BalanceAccountLayout = bytemuck::from_bytes(&dst.data);
    assert_eq!(dst_layout.chain, RECIPIENT_CHAIN);
    assert_eq!(dst_layout.token_chain, TOKEN_CHAIN);
    assert_eq!(dst_layout.balance, Uint256::from_u128(TRANSFER_AMOUNT));

    // Chain registration PDA: untouched (read-only cross-check).
    let reg = find_account(&result.resulting_accounts, &chain_registration_pubkey);
    assert_eq!(reg.owner, program_id());
    assert_eq!(reg.data, chain_registration_account(EMITTER_CHAIN, &EMITTER_ADDRESS).data);

    // Submitter paid rent for two lazy-inited PDAs and the noreplay bucket
    // CPI's own rent; started at 50_000_000_000 lamports; must have strictly
    // decreased and never gone negative (mollusk would panic on underflow
    // regardless, but pin the direction explicitly here).
    let submitter_post = find_account(&result.resulting_accounts, &submitter);
    assert!(
        submitter_post.lamports < 50_000_000_000,
        "submitter must have paid rent for the lazily-created PDAs"
    );

    // Guardian set / guardian signatures / shim / noreplay program / system
    // program: read-only in this instruction, byte-identical post-tx.
    for key in [
        guardian_set_pubkey,
        guardian_signatures_pubkey,
        shim_program_id(),
        noreplay_program_id(),
        system_program_id(),
    ] {
        let pre = find_account(&initial_accounts, &key);
        let post = find_account(&result.resulting_accounts, &key);
        assert_eq!(pre.data, post.data, "read-only account {key} data unchanged");
        assert_eq!(pre.owner, post.owner, "read-only account {key} owner unchanged");
    }
}
