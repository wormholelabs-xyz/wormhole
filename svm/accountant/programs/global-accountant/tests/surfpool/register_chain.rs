//! `register_chain` against the real Verify VAA Shim: a synthetic `RegisterChain`
//! governance VAA, signed by test guardians, registers a foreign chain's Token
//! Bridge emitter. A `submit_vaas` transfer from that emitter succeeds while it
//! is registered. A rotation on a later sequence overwrites the registration in
//! place ("unregistering" the old emitter, since nothing but a matching emitter
//! address is ever accepted): the same emitter's transfers now reject with
//! `UnregisteredEmitter`, and replaying the first `RegisterChain` sequence
//! rejects with `DuplicateRegisterChain`.

use accountant_operational_core::accounts::balance;
use accountant_operational_core::accounts::chain_registration;
use accountant_operational_core::cpi::noreplay::derive_bucket_pda;
use accountant_operational_core::instructions::register_chain::derive_register_chain_pda;
use global_accountant_definitions::{
    ChainRegistrationLayout, GlobalAccountantError, GovernanceHeader, RegisterChainLayout, Uint256,
    GOVERNANCE_EMITTER, REGISTER_CHAIN_ACTION, SOLANA_CHAIN_ID, TOKEN_BRIDGE_GOVERNANCE_MODULE,
};
use solana_instruction::{AccountMeta, Instruction};
use solana_keypair::Keypair;
use solana_signer::Signer;

use crate::common::{
    accountant_image, balance_account, balance_of, double_keccak256, governance_header, layout,
    make_guardians, noreplay_authority_pda, post_signatures_ix, register_chain_body,
    register_chain_ix_data, set_compute_unit_limit_ix, shim_program_id, signature_block,
    signatures_for, submit_vaas_ix_data, system_program_id, transfer_body, ETHEREUM,
    GUARDIAN_COUNT, GUARDIAN_SET_INDEX, NOREPLAY_PROGRAM_ID, QUORUM, TOKEN_ADDRESS,
};
use crate::harness::{
    deploy_programs, fund, send, send_expect_error, set_account, start_surfpool, ProgramImage,
    SurfpoolOptions,
};

const PAYER_LAMPORTS: u64 = 20_000_000_000;
const SEQUENCE_A: u64 = 10;
const SEQUENCE_B: u64 = 11;
const TRANSFER_SEQUENCE_BEFORE: u64 = 0x50;
const TRANSFER_SEQUENCE_AFTER: u64 = 0x51;
const SEED_BALANCE: u128 = 2_000_000;
const TRANSFER_AMOUNT: u128 = 500_000;
/// The shim's `verify_vaa` CPI alone consumes ~196k CU signing with the full
/// guardian quorum, over the 200k default budget.
const REGISTER_CHAIN_CU_LIMIT: u32 = 400_000;
const SUBMIT_VAAS_CU_LIMIT: u32 = 400_000;

fn any_target() -> GovernanceHeader {
    governance_header(TOKEN_BRIDGE_GOVERNANCE_MODULE, REGISTER_CHAIN_ACTION, 0)
}

#[test]
#[ignore = "spawns surfpool subprocess; run via `just e2e`"]
fn surfpool_register_chain_rotate_and_replay() {
    let guard = start_surfpool(SurfpoolOptions::offline("ga-surfpool-register-chain"));
    let rpc = guard.rpc_client();

    let accountant = accountant_image();
    let program_id = accountant.program_id;
    deploy_programs(
        &rpc,
        &[
            accountant,
            ProgramImage::verify_vaa_shim(),
            ProgramImage::noreplay(),
        ],
    );

    let payer = Keypair::new();
    fund(&rpc, &payer.pubkey(), PAYER_LAMPORTS);

    let guardians = make_guardians(GUARDIAN_COUNT, 0x42);
    let (guardian_set, guardian_set_bump) =
        crate::harness::deploy_guardian_set(&rpc, GUARDIAN_SET_INDEX, &guardians);

    let (registration_pda, _) = chain_registration::derive_pda(&program_id, ETHEREUM);

    // Signs `sequence`/`emitter` with the test guardians, posts the signatures through
    // the real shim, and returns the `register_chain` instruction for it.
    let build_registration_ix = |sequence: u64, emitter: [u8; 32]| -> Instruction {
        let body = register_chain_body(
            SOLANA_CHAIN_ID,
            GOVERNANCE_EMITTER,
            sequence,
            any_target(),
            ETHEREUM,
            emitter,
        );
        let digest = double_keccak256(&body);
        let guardian_signatures = Keypair::new();
        send(
            &rpc,
            &format!("post_signatures[register_chain seq={sequence}]"),
            &[post_signatures_ix(
                &payer.pubkey(),
                &guardian_signatures.pubkey(),
                GUARDIAN_SET_INDEX,
                QUORUM,
                &signature_block(&signatures_for(&guardians, &digest, QUORUM)),
            )],
            &[&payer, &guardian_signatures],
        );
        let (register_chain_pda, _) = derive_register_chain_pda(&program_id, sequence);
        Instruction {
            program_id,
            accounts: vec![
                AccountMeta::new(payer.pubkey(), true),
                AccountMeta::new_readonly(shim_program_id(), false),
                AccountMeta::new_readonly(guardian_set, false),
                AccountMeta::new_readonly(guardian_signatures.pubkey(), false),
                AccountMeta::new(registration_pda, false),
                AccountMeta::new_readonly(system_program_id(), false),
                AccountMeta::new(register_chain_pda, false),
            ],
            data: register_chain_ix_data(guardian_set_bump, &body),
        }
    };

    let noreplay_authority = noreplay_authority_pda(&program_id);
    let (source, _) = balance::derive_pda(&program_id, ETHEREUM, ETHEREUM, &TOKEN_ADDRESS);
    let (dest, _) = balance::derive_pda(&program_id, SOLANA_CHAIN_ID, ETHEREUM, &TOKEN_ADDRESS);

    // Signs a Token Bridge transfer from `emitter` on `ETHEREUM` and returns the
    // `submit_vaas` instruction for it. `chain_registration::verify` gates this on
    // `emitter` matching the chain's current registration.
    let build_transfer_ix = |sequence: u64, emitter: [u8; 32]| -> Instruction {
        let body = transfer_body(
            ETHEREUM,
            emitter,
            sequence,
            Uint256::from_u128(TRANSFER_AMOUNT),
            ETHEREUM,
            TOKEN_ADDRESS,
            SOLANA_CHAIN_ID,
        );
        let digest = double_keccak256(&body);
        let guardian_signatures = Keypair::new();
        send(
            &rpc,
            &format!("post_signatures[submit_vaas seq={sequence}]"),
            &[post_signatures_ix(
                &payer.pubkey(),
                &guardian_signatures.pubkey(),
                GUARDIAN_SET_INDEX,
                QUORUM,
                &signature_block(&signatures_for(&guardians, &digest, QUORUM)),
            )],
            &[&payer, &guardian_signatures],
        );
        let (bucket, _) = derive_bucket_pda(&noreplay_authority, ETHEREUM, &emitter, sequence);
        Instruction {
            program_id,
            accounts: vec![
                AccountMeta::new(payer.pubkey(), true),
                AccountMeta::new_readonly(shim_program_id(), false),
                AccountMeta::new_readonly(guardian_set, false),
                AccountMeta::new_readonly(guardian_signatures.pubkey(), false),
                AccountMeta::new(bucket, false),
                AccountMeta::new_readonly(NOREPLAY_PROGRAM_ID, false),
                AccountMeta::new_readonly(noreplay_authority, false),
                AccountMeta::new(source, false),
                AccountMeta::new(dest, false),
                AccountMeta::new_readonly(system_program_id(), false),
                AccountMeta::new_readonly(registration_pda, false),
            ],
            data: submit_vaas_ix_data(guardian_set_bump, &body),
        }
    };

    let emitter_a = [0x77u8; 32];
    let emitter_b = [0xBBu8; 32];

    let ix_a = build_registration_ix(SEQUENCE_A, emitter_a);
    send(
        &rpc,
        "register_chain[a]",
        &[set_compute_unit_limit_ix(REGISTER_CHAIN_CU_LIMIT), ix_a],
        &[&payer],
    );

    let registration = rpc
        .get_account(&registration_pda)
        .expect("registration PDA exists");
    assert_eq!(registration.owner, program_id, "registration PDA owner");
    assert_eq!(
        layout::<ChainRegistrationLayout>(&registration),
        ChainRegistrationLayout::new(ETHEREUM, emitter_a, SEQUENCE_A)
    );
    let (register_chain_pda_a, _) = derive_register_chain_pda(&program_id, SEQUENCE_A);
    let record_a = rpc
        .get_account(&register_chain_pda_a)
        .expect("register_chain record exists");
    assert_eq!(
        layout::<RegisterChainLayout>(&record_a),
        RegisterChainLayout::new(ETHEREUM, emitter_a, SEQUENCE_A)
    );

    // While emitter_a is the registered emitter, a transfer from it goes through.
    // `token_chain == ETHEREUM` makes this a native lock on `source` (credit) and a
    // wrapped mint on `dest` (credit), so both balances rise by `TRANSFER_AMOUNT`.
    set_account(
        &rpc,
        &source,
        &balance_account(
            ETHEREUM,
            ETHEREUM,
            TOKEN_ADDRESS,
            Uint256::from_u128(SEED_BALANCE),
        ),
    );
    set_account(
        &rpc,
        &dest,
        &balance_account(
            SOLANA_CHAIN_ID,
            ETHEREUM,
            TOKEN_ADDRESS,
            Uint256::from_u128(SEED_BALANCE),
        ),
    );
    let transfer_before = build_transfer_ix(TRANSFER_SEQUENCE_BEFORE, emitter_a);
    send(
        &rpc,
        "submit_vaas[registered emitter]",
        &[
            set_compute_unit_limit_ix(SUBMIT_VAAS_CU_LIMIT),
            transfer_before,
        ],
        &[&payer],
    );
    let source_after_transfer = rpc.get_account(&source).expect("source balance PDA exists");
    let dest_after_transfer = rpc.get_account(&dest).expect("dest balance PDA exists");
    assert_eq!(
        balance_of(&source_after_transfer),
        Uint256::from_u128(SEED_BALANCE + TRANSFER_AMOUNT),
        "source balance after transfer"
    );
    assert_eq!(
        balance_of(&dest_after_transfer),
        Uint256::from_u128(SEED_BALANCE + TRANSFER_AMOUNT),
        "dest balance after transfer"
    );

    // Rotation: a later governance VAA overwrites the registration in place.
    let ix_b = build_registration_ix(SEQUENCE_B, emitter_b);
    send(
        &rpc,
        "register_chain[rotate]",
        &[set_compute_unit_limit_ix(REGISTER_CHAIN_CU_LIMIT), ix_b],
        &[&payer],
    );
    let rotated = rpc
        .get_account(&registration_pda)
        .expect("registration PDA exists after rotation");
    assert_eq!(
        layout::<ChainRegistrationLayout>(&rotated),
        ChainRegistrationLayout::new(ETHEREUM, emitter_b, SEQUENCE_B)
    );

    // Post-rotation, a transfer from emitter_a on a fresh sequence rejects: the
    // registration now points at emitter_b.
    let transfer_after = build_transfer_ix(TRANSFER_SEQUENCE_AFTER, emitter_a);
    send_expect_error(
        &rpc,
        "submit_vaas[unregistered emitter]",
        &[
            set_compute_unit_limit_ix(SUBMIT_VAAS_CU_LIMIT),
            transfer_after,
        ],
        &[&payer],
        GlobalAccountantError::UnregisteredEmitter,
    );
    let source_after_rotation = rpc.get_account(&source).expect("source balance PDA exists");
    let dest_after_rotation = rpc.get_account(&dest).expect("dest balance PDA exists");
    assert_eq!(
        balance_of(&source_after_rotation),
        Uint256::from_u128(SEED_BALANCE + TRANSFER_AMOUNT),
        "source balance unchanged by the rejected transfer"
    );
    assert_eq!(
        balance_of(&dest_after_rotation),
        Uint256::from_u128(SEED_BALANCE + TRANSFER_AMOUNT),
        "dest balance unchanged by the rejected transfer"
    );

    // Replaying sequence A's VAA rejects: its RegisterChain PDA already exists.
    let replay = build_registration_ix(SEQUENCE_A, emitter_a);
    send_expect_error(
        &rpc,
        "register_chain[a] replay",
        &[set_compute_unit_limit_ix(REGISTER_CHAIN_CU_LIMIT), replay],
        &[&payer],
        GlobalAccountantError::DuplicateRegisterChain,
    );
}
