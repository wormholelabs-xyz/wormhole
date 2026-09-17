//! `modify_balance` against the real Verify VAA Shim: a synthetic `ModifyBalance`
//! governance VAA, signed by test guardians, creates a balance PDA on `Add`, a
//! second `Add` and a `Subtract` share that PDA, and replaying a payload sequence
//! rejects with `DuplicateModifyBalance`.

use accountant_operational_core::accounts::balance;
use accountant_operational_core::instructions::modify_balance::derive_modify_balance_pda;
use global_accountant_definitions::{
    GlobalAccountantError, GovernanceHeader, ModificationKind, ModifyBalanceLayout, Uint256,
    ACCOUNTANT_GOVERNANCE_MODULE, GOVERNANCE_EMITTER, MODIFY_BALANCE_ACTION, SOLANA_CHAIN_ID,
};
use solana_instruction::{AccountMeta, Instruction};
use solana_keypair::Keypair;
use solana_signer::Signer;

use crate::common::{
    accountant_image, balance_of, core_bridge_program_id, derive_guardian_set_pda,
    double_keccak256, governance_header, guardian_keys, guardian_set_account, make_guardians,
    modify_balance_body, modify_balance_ix_data, post_signatures_ix, set_compute_unit_limit_ix,
    shim_program_id, signature_block, signatures_for, system_program_id, ETHEREUM, GUARDIAN_COUNT,
    GUARDIAN_SET_INDEX, QUORUM, TOKEN_ADDRESS,
};
use crate::harness::{
    deploy_programs, fund, send, send_expect_error, set_account, start_surfpool, ProgramImage,
    SurfpoolOptions,
};

const PAYER_LAMPORTS: u64 = 20_000_000_000;
const REASON: [u8; 32] = *b"audit-log: post-incident credit ";
/// The shim's `verify_vaa` CPI alone consumes ~196k CU signing with the full
/// guardian quorum, over the 200k default budget.
const MODIFY_BALANCE_CU_LIMIT: u32 = 400_000;

fn solana_target() -> GovernanceHeader {
    governance_header(
        ACCOUNTANT_GOVERNANCE_MODULE,
        MODIFY_BALANCE_ACTION,
        SOLANA_CHAIN_ID,
    )
}

fn record_layout(data: &[u8]) -> ModifyBalanceLayout {
    *bytemuck::from_bytes(data)
}

#[test]
#[ignore = "spawns surfpool subprocess; run via `just e2e`"]
fn surfpool_modify_balance_create_delta_and_replay() {
    let guard = start_surfpool(SurfpoolOptions::offline("ga-surfpool-modify-balance"));
    let rpc = guard.rpc_client();

    let accountant = accountant_image();
    let program_id = accountant.program_id;
    deploy_programs(&rpc, &[accountant, ProgramImage::verify_vaa_shim()]);

    let payer = Keypair::new();
    fund(&rpc, &payer.pubkey(), PAYER_LAMPORTS);

    let guardians = make_guardians(GUARDIAN_COUNT, 0x42);
    let (guardian_set, guardian_set_bump) =
        derive_guardian_set_pda(GUARDIAN_SET_INDEX, &core_bridge_program_id());
    set_account(
        &rpc,
        &guardian_set,
        &guardian_set_account(
            GUARDIAN_SET_INDEX,
            &guardian_keys(&guardians),
            0,
            0,
            &core_bridge_program_id(),
        ),
    );

    let (balance_pda, _) = balance::derive_pda(&program_id, ETHEREUM, ETHEREUM, &TOKEN_ADDRESS);

    // Signs `vaa_sequence`/`payload_sequence` with the test guardians, posts the
    // signatures through the real shim, and returns the `modify_balance` instruction.
    let build_modification_ix =
        |vaa_sequence: u64, payload_sequence: u64, kind: ModificationKind, amount: u128| {
            let body = modify_balance_body(
                SOLANA_CHAIN_ID,
                GOVERNANCE_EMITTER,
                vaa_sequence,
                solana_target(),
                payload_sequence,
                ETHEREUM,
                ETHEREUM,
                TOKEN_ADDRESS,
                kind as u8,
                Uint256::from_u128(amount),
                REASON,
            );
            let digest = double_keccak256(&body);
            let guardian_signatures = Keypair::new();
            send(
                &rpc,
                &format!("post_signatures[modify_balance seq={payload_sequence}]"),
                &[post_signatures_ix(
                    &payer.pubkey(),
                    &guardian_signatures.pubkey(),
                    GUARDIAN_SET_INDEX,
                    QUORUM,
                    &signature_block(&signatures_for(&guardians, &digest, QUORUM)),
                )],
                &[&payer, &guardian_signatures],
            );
            let (modify_balance_pda, _) = derive_modify_balance_pda(&program_id, payload_sequence);
            Instruction {
                program_id,
                accounts: vec![
                    AccountMeta::new(payer.pubkey(), true),
                    AccountMeta::new_readonly(shim_program_id(), false),
                    AccountMeta::new_readonly(guardian_set, false),
                    AccountMeta::new_readonly(guardian_signatures.pubkey(), false),
                    AccountMeta::new(balance_pda, false),
                    AccountMeta::new_readonly(system_program_id(), false),
                    AccountMeta::new(modify_balance_pda, false),
                ],
                data: modify_balance_ix_data(guardian_set_bump, &body),
            }
        };

    let assert_balance = |expected: u128, label: &str| {
        let account = rpc.get_account(&balance_pda).expect("balance PDA exists");
        assert_eq!(account.owner, program_id, "{label} balance PDA owner");
        assert_eq!(
            balance_of(&account),
            Uint256::from_u128(expected),
            "{label} balance"
        );
    };
    let assert_record =
        |payload_sequence: u64, kind: ModificationKind, amount: u128, label: &str| {
            let (pda, _) = derive_modify_balance_pda(&program_id, payload_sequence);
            let record = rpc.get_account(&pda).expect("modify_balance record exists");
            assert_eq!(record.owner, program_id, "{label} record owner");
            assert_eq!(
                record_layout(&record.data),
                ModifyBalanceLayout::new(
                    kind,
                    ETHEREUM,
                    ETHEREUM,
                    payload_sequence,
                    TOKEN_ADDRESS,
                    Uint256::from_u128(amount),
                    REASON,
                ),
                "{label} record"
            );
        };

    let create = build_modification_ix(0x10, 200, ModificationKind::Add, 1_000_000);
    send(
        &rpc,
        "modify_balance[create]",
        &[set_compute_unit_limit_ix(MODIFY_BALANCE_CU_LIMIT), create],
        &[&payer],
    );
    assert_balance(1_000_000, "create");
    assert_record(200, ModificationKind::Add, 1_000_000, "create");

    let add = build_modification_ix(0x11, 201, ModificationKind::Add, 500);
    send(
        &rpc,
        "modify_balance[add]",
        &[set_compute_unit_limit_ix(MODIFY_BALANCE_CU_LIMIT), add],
        &[&payer],
    );
    assert_balance(1_000_500, "add");

    let subtract = build_modification_ix(0x12, 202, ModificationKind::Subtract, 1_500);
    send(
        &rpc,
        "modify_balance[subtract]",
        &[set_compute_unit_limit_ix(MODIFY_BALANCE_CU_LIMIT), subtract],
        &[&payer],
    );
    assert_balance(999_000, "subtract");

    // Subtracting more than the current balance rejects and leaves the balance
    // and its record untouched: the instruction fails before any state commits.
    let overdraft = build_modification_ix(0x13, 203, ModificationKind::Subtract, 2_000_000);
    send_expect_error(
        &rpc,
        "modify_balance[subtract] underflow",
        &[
            set_compute_unit_limit_ix(MODIFY_BALANCE_CU_LIMIT),
            overdraft,
        ],
        &[&payer],
        GlobalAccountantError::ModifyBalanceUnderflow,
    );
    assert_balance(999_000, "post-underflow-rejection");

    // Replaying payload sequence 200 rejects: its ModifyBalance PDA already exists.
    let replay = build_modification_ix(0x10, 200, ModificationKind::Add, 1_000_000);
    send_expect_error(
        &rpc,
        "modify_balance[create] replay",
        &[set_compute_unit_limit_ix(MODIFY_BALANCE_CU_LIMIT), replay],
        &[&payer],
        GlobalAccountantError::DuplicateModifyBalance,
    );
}
