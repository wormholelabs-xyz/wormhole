//! Two-step surfpool e2e for `upgrade_contract`.
//!
//! Step 1 (`just e2e-upgrade-deploy`): start a detached offline surfpool, deploy the
//! accountant with its upgrade authority moved to the `[b"upgrade"]` PDA, deploy the
//! NoReplay and Verify-VAA-Shim programs, seed a test guardian set, and stage the
//! buffer holding the new program image. Surfpool keeps running after this exits.
//!
//! Step 2 (`just e2e-upgrade-submit`): sign the accountant `UpgradeContract` governance
//! VAA (action 2) with the test guardians, post the signatures through the real shim,
//! submit `upgrade_contract`, then assert the loader rewrote the program-data account,
//! the buffer closed into the spill, NoReplay marked the sequence, and a replay of the
//! same VAA rejects with `AlreadyAccounted`.
//!
//! `just e2e-upgrade-stop` kills the surfpool instance from step 1.

#![allow(clippy::too_many_arguments)]

use std::path::PathBuf;
use std::time::Duration;

use global_accountant_definitions::{
    GlobalAccountantError, NoReplayBitmapAccount, ACCOUNTANT_GOVERNANCE_MODULE,
    GOVERNANCE_EMITTER, NOREPLAY_AUTHORITY_SEED_PREFIX, SOLANA_CHAIN_ID, UPGRADE_CONTRACT_ACTION,
    VERIFY_VAA_SHIM_PROGRAM_ID,
};
use solana_instruction::{AccountMeta, Instruction};
use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use solana_signer::Signer;
use solana_system_interface::program as system_program;
use solana_transaction::Transaction;

mod common;
use accountant_operational_core::cpi::loader::derive_upgrade_authority;
use accountant_operational_core::cpi::noreplay::derive_bucket_pda;
use common::{
    await_confirmed, deploy_program, derive_guardian_set_pda, double_keccak256, governance_header,
    hex_encode, make_guardians, noreplay_so_path, rpc_call, sign_digest, so_path, start_surfpool,
    upgrade_contract_body, upgrade_contract_ix_data, SurfpoolOptions, GUARDIAN_COUNT,
    GUARDIAN_SET_INDEX, NOREPLAY_PROGRAM_ID, QUORUM,
};

/// Governance sequence consumed by the upgrade VAA.
const SEQUENCE: u64 = 1;

/// Buffer address carried in the VAA payload as `new_contract`.
const BUFFER: Pubkey = Pubkey::new_from_array([0xBF; 32]);

const POST_SIGNATURES_SELECTOR: [u8; 8] = [0x8a, 0x02, 0x35, 0xa6, 0x2d, 0x4d, 0x89, 0x33];

const COMPUTE_BUDGET_PROGRAM_ID: Pubkey = Pubkey::new_from_array([
    0x03, 0x06, 0x46, 0x6f, 0xe5, 0x21, 0x17, 0x32, 0xff, 0xec, 0xad, 0xba, 0x72, 0xc3, 0x9b, 0xe7,
    0xbc, 0x8c, 0xe5, 0xbb, 0xc5, 0xf7, 0x12, 0x6b, 0x2c, 0x43, 0x9b, 0x3a, 0x40, 0x00, 0x00, 0x00,
]);

const UPGRADE_CU_LIMIT: u32 = 1_000_000;

fn loader_id() -> Pubkey {
    Pubkey::from_str_const("BPFLoaderUpgradeab1e11111111111111111111111")
}

fn rent_sysvar_id() -> Pubkey {
    Pubkey::from_str_const("SysvarRent111111111111111111111111111111111")
}

fn clock_sysvar_id() -> Pubkey {
    Pubkey::from_str_const("SysvarC1ock11111111111111111111111111111111")
}

fn ga_program_id() -> Pubkey {
    Pubkey::new_from_array(global_accountant::ID.to_bytes())
}

fn shim_program_id() -> Pubkey {
    Pubkey::new_from_array(VERIFY_VAA_SHIM_PROGRAM_ID)
}

fn derive_program_data(program_id: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(&[program_id.as_ref()], &loader_id()).0
}

fn state_file() -> PathBuf {
    so_path("global_accountant")
        .parent()
        .and_then(|p| p.parent())
        .expect("target dir from .so path")
        .join("e2e-upgrade-contract.json")
}

fn set_account(rpc_url: &str, key: &Pubkey, owner: &Pubkey, lamports: u64, data: &[u8]) {
    let resp = rpc_call(
        rpc_url,
        "surfnet_setAccount",
        serde_json::json!([
            key.to_string(),
            {
                "lamports": lamports,
                "owner": owner.to_string(),
                "executable": false,
                "rent_epoch": 0u64,
                "data": hex_encode(data),
            }
        ]),
    );
    assert!(
        resp.get("error").is_none(),
        "surfnet_setAccount failed for {key}: {resp}"
    );
}

/// BPF upgradeable loader `Buffer` account: tag 1, authority present, ELF payload.
fn buffer_data(authority: &Pubkey, elf: &[u8]) -> Vec<u8> {
    let mut data = Vec::with_capacity(37 + elf.len());
    data.extend_from_slice(&1u32.to_le_bytes());
    data.push(1);
    data.extend_from_slice(authority.as_ref());
    data.extend_from_slice(elf);
    data
}

fn upgrade_body() -> Vec<u8> {
    upgrade_contract_body(
        SOLANA_CHAIN_ID,
        GOVERNANCE_EMITTER,
        SEQUENCE,
        governance_header(
            ACCOUNTANT_GOVERNANCE_MODULE,
            UPGRADE_CONTRACT_ACTION,
            SOLANA_CHAIN_ID,
        ),
        BUFFER.to_bytes(),
    )
}

/// Kill the surfpool recorded in the state file, so re-deploys replace instead of orphan.
fn kill_previous_instance() {
    let Ok(raw) = std::fs::read_to_string(state_file()) else {
        return;
    };
    let Ok(state) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return;
    };
    if let Some(pid) = state["surfpool_pid"].as_u64() {
        eprintln!("[upgrade-e2e] stopping previous surfpool pid={pid}");
        let _ = std::process::Command::new("kill").arg(pid.to_string()).status();
    }
    let _ = std::fs::remove_file(state_file());
}

/// Step 1: spin up surfpool and stage everything the upgrade VAA needs.
#[test]
#[ignore = "starts a long-lived surfpool; run via `just e2e-upgrade-deploy`"]
fn deploy_upgradeable_accountant() {
    let ga_elf = std::fs::read(so_path("global_accountant")).unwrap_or_else(|e| {
        panic!("read global_accountant.so: {e}. Run `just build` first.")
    });
    let noreplay_elf = std::fs::read(noreplay_so_path()).expect("read solana_noreplay.so");
    let shim_elf = accountant_test_fixtures::VERIFY_VAA_SHIM_SO.bytes.to_vec();

    kill_previous_instance();

    let guard = start_surfpool(SurfpoolOptions::offline_detached("ga-surfpool-upgrade"));
    let rpc_url = guard.rpc_url();
    let rpc = guard.rpc_client();

    let program_id = ga_program_id();
    deploy_program(&rpc_url, &program_id, &ga_elf);
    deploy_program(&rpc_url, &NOREPLAY_PROGRAM_ID, &noreplay_elf);
    deploy_program(&rpc_url, &shim_program_id(), &shim_elf);

    // Move the deployed program's upgrade authority to the `[b"upgrade"]` PDA — the
    // on-chain equivalent of `solana program set-upgrade-authority`.
    let (upgrade_authority, _) = derive_upgrade_authority(&program_id);
    let program_account = rpc.get_account(&program_id).expect("program account");
    assert_eq!(program_account.owner, loader_id(), "program owner");
    assert_eq!(
        u32::from_le_bytes(program_account.data[..4].try_into().unwrap()),
        2,
        "loader state tag Program"
    );
    let program_data = Pubkey::new_from_array(program_account.data[4..36].try_into().unwrap());
    assert_eq!(
        program_data,
        derive_program_data(&program_id),
        "program points at the canonical program-data address"
    );
    let mut pd = rpc.get_account(&program_data).expect("program-data account");
    assert_eq!(
        u32::from_le_bytes(pd.data[..4].try_into().unwrap()),
        3,
        "loader state tag ProgramData"
    );
    pd.data[12] = 1;
    pd.data[13..45].copy_from_slice(upgrade_authority.as_ref());
    set_account(&rpc_url, &program_data, &loader_id(), pd.lamports, &pd.data);
    eprintln!(
        "[upgrade-e2e] program={program_id} program_data={program_data} \
         upgrade_authority={upgrade_authority}"
    );

    // Guardian set the shim verifies against: test guardians at the Core Bridge PDA.
    let guardians = make_guardians(GUARDIAN_COUNT, 0x42);
    let keys: Vec<[u8; 20]> = guardians.iter().map(|g| g.eth_address).collect();
    let (gs_pda, gs_bump) = derive_guardian_set_pda(
        GUARDIAN_SET_INDEX,
        &Pubkey::new_from_array(global_accountant_definitions::CORE_BRIDGE_PROGRAM_ID),
    );
    let gs = common::guardian_set_account(
        GUARDIAN_SET_INDEX,
        &keys,
        0,
        0,
        &Pubkey::new_from_array(global_accountant_definitions::CORE_BRIDGE_PROGRAM_ID),
    );
    set_account(&rpc_url, &gs_pda, &gs.owner, gs.lamports, &gs.data);
    eprintln!("[upgrade-e2e] guardian_set={gs_pda} bump={gs_bump} index={GUARDIAN_SET_INDEX}");

    // Buffer holding the replacement image, authority = upgrade PDA.
    let buffer = buffer_data(&upgrade_authority, &ga_elf);
    set_account(&rpc_url, &BUFFER, &loader_id(), 5_000_000_000, &buffer);
    eprintln!(
        "[upgrade-e2e] buffer={BUFFER} elf={} bytes, authority={upgrade_authority}",
        ga_elf.len()
    );

    let state = serde_json::json!({
        "rpc_url": rpc_url,
        "surfpool_pid": guard.pid(),
    });
    std::fs::write(state_file(), state.to_string()).expect("write state file");
    eprintln!(
        "[upgrade-e2e] surfpool pid={} rpc={rpc_url}; state at {}",
        guard.pid(),
        state_file().display()
    );
    eprintln!("[upgrade-e2e] deploy step complete; run `just e2e-upgrade-submit` next");
    guard.detach();
}

/// Step 2: sign the governance VAA, submit `upgrade_contract`, verify the upgrade.
#[test]
#[ignore = "needs the surfpool from `just e2e-upgrade-deploy`; run via `just e2e-upgrade-submit`"]
fn submit_upgrade_vaa() {
    let state_raw = std::fs::read_to_string(state_file()).unwrap_or_else(|e| {
        panic!(
            "read {}: {e}. Run `just e2e-upgrade-deploy` first.",
            state_file().display()
        )
    });
    let state: serde_json::Value = serde_json::from_str(&state_raw).expect("state JSON");
    let rpc_url = state["rpc_url"].as_str().expect("rpc_url").to_string();
    let rpc = solana_client::rpc_client::RpcClient::new_with_commitment(
        rpc_url.clone(),
        solana_commitment_config::CommitmentConfig::confirmed(),
    );
    rpc.get_health().unwrap_or_else(|e| {
        panic!("surfpool at {rpc_url} not healthy: {e}. Run `just e2e-upgrade-deploy` first.")
    });

    let program_id = ga_program_id();
    let program_data = derive_program_data(&program_id);
    let (upgrade_authority, _) = derive_upgrade_authority(&program_id);
    let (noreplay_authority, _) =
        Pubkey::find_program_address(&[NOREPLAY_AUTHORITY_SEED_PREFIX], &program_id);
    let (bucket_pda, _) = derive_bucket_pda(
        &noreplay_authority,
        SOLANA_CHAIN_ID,
        &GOVERNANCE_EMITTER,
        SEQUENCE,
    );
    let (gs_pda, gs_bump) = derive_guardian_set_pda(
        GUARDIAN_SET_INDEX,
        &Pubkey::new_from_array(global_accountant_definitions::CORE_BRIDGE_PROGRAM_ID),
    );

    if let Ok(bucket) = rpc.get_account(&bucket_pda) {
        if NoReplayBitmapAccount::from_bytes(&bucket.data).is_some_and(|b| b.is_marked(SEQUENCE)) {
            panic!(
                "sequence {SEQUENCE} already consumed on this surfpool — \
                 re-run `just e2e-upgrade-deploy` for a fresh instance"
            );
        }
    }

    let payer = Keypair::new();
    let airdrop = rpc
        .request_airdrop(&payer.pubkey(), 20_000_000_000)
        .expect("airdrop payer");
    await_confirmed("airdrop", Duration::from_secs(10), || {
        rpc.confirm_transaction(&airdrop)
    });

    let body = upgrade_body();
    let digest = double_keccak256(&body);
    eprintln!(
        "[upgrade-e2e] VAA body: sequence={SEQUENCE} new_contract={BUFFER} digest={}",
        hex_encode(&digest)
    );

    // Guardian signatures through the real shim: PostSignatures, then the accountant's
    // verify CPI reads the resulting GuardianSignatures account.
    let guardians = make_guardians(GUARDIAN_COUNT, 0x42);
    let mut sig_block = Vec::with_capacity(QUORUM as usize * 66);
    for i in 0..QUORUM {
        sig_block.push(i);
        sig_block.extend_from_slice(&sign_digest(&guardians[i as usize], &digest));
    }
    let guardian_signatures_kp = Keypair::new();
    let mut post_data = Vec::with_capacity(8 + 9 + sig_block.len());
    post_data.extend_from_slice(&POST_SIGNATURES_SELECTOR);
    post_data.extend_from_slice(&GUARDIAN_SET_INDEX.to_le_bytes());
    post_data.push(QUORUM);
    post_data.extend_from_slice(&(QUORUM as u32).to_le_bytes());
    post_data.extend_from_slice(&sig_block);
    let post_ix = Instruction {
        program_id: shim_program_id(),
        accounts: vec![
            AccountMeta::new(payer.pubkey(), true),
            AccountMeta::new(guardian_signatures_kp.pubkey(), true),
            AccountMeta::new_readonly(system_program::ID, false),
        ],
        data: post_data,
    };
    let blockhash = rpc.get_latest_blockhash().expect("blockhash post_signatures");
    let post_tx = Transaction::new_signed_with_payer(
        &[post_ix],
        Some(&payer.pubkey()),
        &[&payer, &guardian_signatures_kp],
        blockhash,
    );
    let post_sig = rpc
        .send_and_confirm_transaction(&post_tx)
        .expect("PostSignatures send_and_confirm");
    eprintln!("[upgrade-e2e] PostSignatures tx={post_sig}");

    let pd_before = rpc.get_account(&program_data).expect("program-data before");
    let slot_before = u64::from_le_bytes(pd_before.data[4..12].try_into().unwrap());
    let spill = Pubkey::new_unique();

    let metas = vec![
        AccountMeta::new(payer.pubkey(), true),
        AccountMeta::new_readonly(shim_program_id(), false),
        AccountMeta::new_readonly(gs_pda, false),
        AccountMeta::new_readonly(guardian_signatures_kp.pubkey(), false),
        AccountMeta::new(bucket_pda, false),
        AccountMeta::new_readonly(NOREPLAY_PROGRAM_ID, false),
        AccountMeta::new_readonly(noreplay_authority, false),
        AccountMeta::new_readonly(system_program::ID, false),
        AccountMeta::new_readonly(upgrade_authority, false),
        AccountMeta::new(spill, false),
        AccountMeta::new(BUFFER, false),
        AccountMeta::new(program_data, false),
        AccountMeta::new(program_id, false),
        AccountMeta::new_readonly(rent_sysvar_id(), false),
        AccountMeta::new_readonly(clock_sysvar_id(), false),
        AccountMeta::new_readonly(loader_id(), false),
    ];
    let mut cu_data = Vec::with_capacity(5);
    cu_data.push(0x02);
    cu_data.extend_from_slice(&UPGRADE_CU_LIMIT.to_le_bytes());
    let cu_ix = Instruction {
        program_id: COMPUTE_BUDGET_PROGRAM_ID,
        accounts: vec![],
        data: cu_data,
    };
    let upgrade_ix = Instruction::new_with_bytes(
        program_id,
        &upgrade_contract_ix_data(gs_bump, &body),
        metas.clone(),
    );
    let blockhash = rpc.get_latest_blockhash().expect("blockhash upgrade");
    let tx = Transaction::new_signed_with_payer(
        &[cu_ix.clone(), upgrade_ix],
        Some(&payer.pubkey()),
        &[&payer],
        blockhash,
    );
    let sig = rpc
        .send_and_confirm_transaction(&tx)
        .expect("upgrade_contract send_and_confirm");
    eprintln!("[upgrade-e2e] upgrade_contract tx={sig}");

    let pd_after = rpc.get_account(&program_data).expect("program-data after");
    assert_eq!(
        u32::from_le_bytes(pd_after.data[..4].try_into().unwrap()),
        3,
        "program-data state tag"
    );
    let slot_after = u64::from_le_bytes(pd_after.data[4..12].try_into().unwrap());
    assert!(
        slot_after > slot_before,
        "program-data slot advances: {slot_before} -> {slot_after}"
    );
    let ga_elf = std::fs::read(so_path("global_accountant")).expect("read global_accountant.so");
    assert_eq!(
        &pd_after.data[45..45 + ga_elf.len()],
        &ga_elf[..],
        "program-data carries the buffer image"
    );

    let buffer_after = rpc.get_account(&BUFFER);
    let buffer_closed = match &buffer_after {
        Ok(acct) => acct.lamports == 0 || acct.data.len() < 37,
        Err(_) => true,
    };
    assert!(buffer_closed, "buffer closed by the loader: {buffer_after:?}");
    let spill_after = rpc.get_account(&spill).expect("spill after");
    assert!(
        spill_after.lamports > 0,
        "spill received the buffer lamports"
    );
    eprintln!(
        "[upgrade-e2e] slot {slot_before} -> {slot_after}; spill received {} lamports",
        spill_after.lamports
    );

    let bucket = rpc.get_account(&bucket_pda).expect("noreplay bucket after");
    assert_eq!(bucket.owner, NOREPLAY_PROGRAM_ID, "bucket owner");
    assert!(
        NoReplayBitmapAccount::from_bytes(&bucket.data)
            .expect("bucket layout")
            .is_marked(SEQUENCE),
        "noreplay marked sequence {SEQUENCE}"
    );

    // Replay of the same VAA must reject with AlreadyAccounted — and this second
    // invocation also proves the upgraded image executes.
    let replay_ix =
        Instruction::new_with_bytes(program_id, &upgrade_contract_ix_data(gs_bump, &body), metas);
    let blockhash = rpc.get_latest_blockhash().expect("blockhash replay");
    let replay_tx = Transaction::new_signed_with_payer(
        &[cu_ix, replay_ix],
        Some(&payer.pubkey()),
        &[&payer],
        blockhash,
    );
    let expected = format!("{:#x}", GlobalAccountantError::AlreadyAccounted as u64);
    match rpc.send_and_confirm_transaction(&replay_tx) {
        Ok(sig) => panic!("replay unexpectedly confirmed: tx={sig}"),
        Err(e) => {
            let msg = e.to_string();
            assert!(
                msg.contains("custom program error") && msg.contains(&expected),
                "expected AlreadyAccounted ({expected}); got: {msg}"
            );
            eprintln!("[upgrade-e2e] replay rejected with AlreadyAccounted");
        }
    }

    eprintln!("[upgrade-e2e] all phases green");
}
