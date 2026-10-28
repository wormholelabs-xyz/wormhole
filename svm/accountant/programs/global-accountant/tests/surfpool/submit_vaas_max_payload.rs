//! `submit_vaas` with a `TransferWithPayload` at the payload cap, sent as a transaction v1.
//! One byte over the cap fails with `TransferPayloadTooLarge`.

use accountant_operational_core::accounts::balance;
use accountant_operational_core::accounts::chain_registration;
use accountant_operational_core::cpi::noreplay::derive_bucket_pda;
use global_accountant_definitions::{GlobalAccountantError, Uint256, MAX_TRANSFER_PAYLOAD_LEN};
use solana_instruction::{AccountMeta, Instruction};
use solana_keypair::Keypair;
use solana_signer::Signer;

use crate::common::{
    accountant_image, assert_bucket_marked, balance_account, balance_of,
    chain_registration_account, double_keccak256, instructions_sysvar_meta, make_guardians,
    noreplay_authority_pda, post_signatures_ix, shim_program_id, signature_block, signatures_for,
    submit_vaas_ix_data, system_program_id, transfer_body, with_transfer_payload, ETHEREUM,
    GUARDIAN_COUNT, GUARDIAN_SET_INDEX, NOREPLAY_PROGRAM_ID, QUORUM, SOLANA, TOKEN_ADDRESS,
};
use crate::harness::{
    assert_canonical_log_in_tx, deploy_guardian_set, deploy_programs, fund, send,
    send_expect_error, set_account, start_surfpool, ComputeUnitLimit, ProgramImage,
    SurfpoolOptions,
};

const PAYER_LAMPORTS: u64 = 20_000_000_000;
const SUBMIT_VAAS_CU_LIMIT: u32 = 400_000;
const EMITTER: [u8; 32] = [0x5Au8; 32];
const SEED_BALANCE: u128 = 2_000_000;
const TRANSFER_AMOUNT: u128 = 500_000;

/// Label, sequence, payload length, expected error.
type Case = (&'static str, u64, usize, Option<GlobalAccountantError>);

#[test]
#[ignore = "spawns surfpool subprocess; run via `just e2e`"]
fn surfpool_submit_vaas_payload_cap() {
    let guard = start_surfpool(SurfpoolOptions::offline("ga-surfpool-max-payload"));
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
    let guardians = make_guardians(GUARDIAN_COUNT, 0x43);
    let (guardian_set, guardian_set_bump) =
        deploy_guardian_set(&rpc, GUARDIAN_SET_INDEX, &guardians);

    let noreplay_authority = noreplay_authority_pda(&program_id);
    let (source, _) = balance::derive_pda(&program_id, ETHEREUM, ETHEREUM, &TOKEN_ADDRESS);
    let (dest, _) = balance::derive_pda(&program_id, SOLANA, ETHEREUM, &TOKEN_ADDRESS);
    let (registration, _) = chain_registration::derive_pda(&program_id, ETHEREUM);
    for (pda, chain) in [(source, ETHEREUM), (dest, SOLANA)] {
        set_account(
            &rpc,
            &pda,
            &balance_account(
                chain,
                ETHEREUM,
                TOKEN_ADDRESS,
                Uint256::from_u128(SEED_BALANCE),
            ),
        );
    }
    set_account(
        &rpc,
        &registration,
        &chain_registration_account(ETHEREUM, EMITTER),
    );

    let cases: [Case; 2] = [
        ("payload at cap", 0x60, MAX_TRANSFER_PAYLOAD_LEN, None),
        (
            "payload over cap",
            0x61,
            MAX_TRANSFER_PAYLOAD_LEN + 1,
            Some(GlobalAccountantError::TransferPayloadTooLarge),
        ),
    ];
    for (label, sequence, payload_len, expected) in cases {
        let body = with_transfer_payload(
            transfer_body(
                ETHEREUM,
                EMITTER,
                sequence,
                Uint256::from_u128(TRANSFER_AMOUNT),
                ETHEREUM,
                TOKEN_ADDRESS,
                SOLANA,
            ),
            &vec![0xC3u8; payload_len],
        );
        let digest = double_keccak256(&body);
        let guardian_signatures = Keypair::new();
        send(
            &rpc,
            &format!("post_signatures[{label}]"),
            &[post_signatures_ix(
                &payer.pubkey(),
                &guardian_signatures.pubkey(),
                GUARDIAN_SET_INDEX,
                QUORUM,
                &signature_block(&signatures_for(&guardians, &digest, QUORUM)),
            )],
            None,
            &[&payer, &guardian_signatures],
        );
        let (bucket, _) = derive_bucket_pda(&noreplay_authority, ETHEREUM, &EMITTER, sequence);
        let submit = Instruction {
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
                AccountMeta::new_readonly(registration, false),
                instructions_sysvar_meta(),
            ],
            data: submit_vaas_ix_data(guardian_set_bump, &body),
        };
        let cu_limit = Some(ComputeUnitLimit::new(SUBMIT_VAAS_CU_LIMIT));
        let label = format!("submit_vaas[{label}]");
        match expected {
            None => {
                let sig = send(&rpc, &label, &[submit], cu_limit, &[&payer]);
                let bucket_after = rpc.get_account(&bucket).expect("bucket PDA exists");
                assert_bucket_marked(&bucket_after, sequence);
                assert_canonical_log_in_tx(
                    &rpc,
                    &sig,
                    ETHEREUM,
                    &EMITTER,
                    sequence,
                    &digest,
                    GUARDIAN_SET_INDEX,
                );
            }
            Some(error) => {
                send_expect_error(&rpc, &label, &[submit], cu_limit, &[&payer], error);
            }
        }
    }

    // Only the at-cap transfer moved balances.
    for (label, pda) in [("source", source), ("dest", dest)] {
        let after = rpc.get_account(&pda).expect("balance PDA exists");
        assert_eq!(
            balance_of(&after),
            Uint256::from_u128(SEED_BALANCE + TRANSFER_AMOUNT),
            "{label} balance"
        );
    }
}
