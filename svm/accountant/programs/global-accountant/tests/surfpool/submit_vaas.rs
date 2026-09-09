//! `submit_vaas` with a real mainnet Token Bridge transfer VAA against a
//! mainnet fork: signatures go through the real Verify VAA Shim, then the
//! accountant moves balances, marks NoReplay, and emits one commit-log record.
//!
//! The fixture is signed by guardian set 6, which has since expired on
//! mainnet. The test rewrites the forked set 6 account with
//! `expiration_time = 0` so the fixture stays valid.

use accountant_operational_core::accounts::chain_registration;
use accountant_operational_core::cpi::noreplay::derive_bucket_pda;
use global_accountant::instructions::transfer::derive_balance_account_pda;
use global_accountant_definitions::{TokenBridgeTransfer, Uint256, VaaBodyHeader};
use solana_instruction::{AccountMeta, Instruction};
use solana_keypair::Keypair;
use solana_signer::Signer;

use crate::common::{
    assert_bucket_marked, balance_account, balance_of, chain_registration_account,
    core_bridge_program_id, derive_guardian_set_pda, double_keccak256,
    guardian_set_with_expiration, noreplay_authority_pda, post_signatures_ix,
    set_compute_unit_limit_ix, shim_program_id, submit_vaas_ix_data, system_program_id,
    NOREPLAY_PROGRAM_ID,
};
use crate::harness::{
    assert_canonical_log_in_tx, deploy_programs, fund, send, set_account, start_surfpool,
    ProgramImage, SurfpoolOptions,
};

const SUBMIT_VAAS_CU_LIMIT: u32 = 400_000;
const PAYER_LAMPORTS: u64 = 20_000_000_000;
const SEED_HEADROOM: u128 = 1_000_000_000;

fn datasource_rpc_url() -> String {
    std::env::var("GA_E2E_DATASOURCE_RPC")
        .unwrap_or_else(|_| "https://api.mainnet-beta.solana.com".to_string())
}

#[test]
#[ignore = "spawns surfpool subprocess; run via `just e2e`"]
fn surfpool_submit_vaas_token_bridge_transfer() {
    let vaa = &accountant_test_fixtures::MAINNET_TRANSFER_SEQ1395207;
    let body = vaa.body();
    let (header, payload) = VaaBodyHeader::split(body).expect("fixture body");
    let transfer: &TokenBridgeTransfer = bytemuck::from_bytes(payload);
    assert_eq!(transfer.action, 0x01, "fixture carries a Transfer action");
    assert_eq!(
        vaa.guardian_set_index(),
        6,
        "fixture is signed by guardian set 6"
    );

    let emitter_chain = header.emitter_chain();
    let emitter_address = header.emitter_address;
    let sequence = header.sequence();
    let digest = double_keccak256(body);
    let amount = Uint256::from_be_bytes(transfer.amount);
    let token_chain = u16::from_be_bytes(transfer.token_chain);
    let recipient_chain = u16::from_be_bytes(transfer.recipient_chain);

    let guard = start_surfpool(SurfpoolOptions::mainnet_fork(
        "ga-surfpool-submit-vaas",
        datasource_rpc_url(),
    ));
    let rpc = guard.rpc_client();

    let accountant = ProgramImage::accountant();
    let program_id = accountant.program_id;
    deploy_programs(&rpc, &[accountant, ProgramImage::noreplay()]);

    let payer = Keypair::new();
    fund(&rpc, &payer.pubkey(), PAYER_LAMPORTS);

    // Lazy-fetch the shim and Core Bridge from mainnet so the fork holds them.
    rpc.get_account(&shim_program_id())
        .expect("shim lazy-fetch");
    rpc.get_account(&core_bridge_program_id())
        .expect("Core Bridge lazy-fetch");

    let (guardian_set, guardian_set_bump) =
        derive_guardian_set_pda(vaa.guardian_set_index(), &core_bridge_program_id());
    let fetched = rpc
        .get_account(&guardian_set)
        .expect("GuardianSet PDA lazy-fetch");
    assert_eq!(fetched.owner, core_bridge_program_id(), "GuardianSet owner");
    set_account(
        &rpc,
        &guardian_set,
        &guardian_set_with_expiration(&fetched, 0),
    );

    let guardian_signatures = Keypair::new();
    send(
        &rpc,
        "post_signatures",
        &[post_signatures_ix(
            &payer.pubkey(),
            &guardian_signatures.pubkey(),
            vaa.guardian_set_index(),
            vaa.signature_count(),
            vaa.signatures(),
        )],
        &[&payer, &guardian_signatures],
    );

    let noreplay_authority = noreplay_authority_pda(&program_id);
    let (bucket, _) = derive_bucket_pda(
        &noreplay_authority,
        emitter_chain,
        &emitter_address,
        sequence,
    );
    let (source, _) = derive_balance_account_pda(
        &program_id,
        emitter_chain,
        token_chain,
        &transfer.token_address,
    );
    let (dest, _) = derive_balance_account_pda(
        &program_id,
        recipient_chain,
        token_chain,
        &transfer.token_address,
    );
    let (chain_registration_pda, _) = chain_registration::derive_pda(&program_id, emitter_chain);

    let seed_balance = amount
        .checked_add(Uint256::from_u128(SEED_HEADROOM))
        .expect("seed balance fits");
    for (pda, chain) in [(source, emitter_chain), (dest, recipient_chain)] {
        set_account(
            &rpc,
            &pda,
            &balance_account(chain, token_chain, transfer.token_address, seed_balance),
        );
    }
    set_account(
        &rpc,
        &chain_registration_pda,
        &chain_registration_account(emitter_chain, emitter_address),
    );

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
            AccountMeta::new_readonly(chain_registration_pda, false),
        ],
        data: submit_vaas_ix_data(guardian_set_bump, body),
    };
    let sig = send(
        &rpc,
        "submit_vaas",
        &[set_compute_unit_limit_ix(SUBMIT_VAAS_CU_LIMIT), submit],
        &[&payer],
    );

    let bucket_after = rpc.get_account(&bucket).expect("bucket PDA exists");
    assert_bucket_marked(&bucket_after, sequence);
    assert_canonical_log_in_tx(
        &rpc,
        &sig,
        emitter_chain,
        &emitter_address,
        sequence,
        &digest,
        vaa.guardian_set_index(),
    );

    let expected_balance = seed_balance
        .checked_sub(amount)
        .expect("seed balance covers the transfer");
    for (label, pda) in [("source", source), ("dest", dest)] {
        let after = rpc.get_account(&pda).expect("balance PDA exists");
        assert_eq!(after.owner, program_id, "{label} balance PDA owner");
        assert_eq!(balance_of(&after), expected_balance, "{label} balance");
    }
}
