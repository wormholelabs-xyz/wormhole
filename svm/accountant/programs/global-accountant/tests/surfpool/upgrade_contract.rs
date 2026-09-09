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

use std::path::PathBuf;

use accountant_operational_core::cpi::loader::derive_upgrade_authority;
use accountant_operational_core::cpi::noreplay::derive_bucket_pda;
use global_accountant_definitions::{
    GlobalAccountantError, ACCOUNTANT_GOVERNANCE_MODULE, GOVERNANCE_EMITTER, SOLANA_CHAIN_ID,
    UPGRADE_CONTRACT_ACTION,
};
use solana_account::Account;
use solana_instruction::{AccountMeta, Instruction};
use solana_keypair::Keypair;
use solana_loader_v3_interface::state::UpgradeableLoaderState;
use solana_pubkey::Pubkey;
use solana_signer::Signer;

use crate::common::{
    assert_bucket_marked, clock_sysvar_id, core_bridge_program_id, derive_guardian_set_pda,
    double_keccak256, governance_header, guardian_keys, guardian_set_account, loader_v3_id,
    make_guardians, noreplay_authority_pda, post_signatures_ix, rent_sysvar_id,
    set_compute_unit_limit_ix, shim_program_id, signature_block, signatures_for, system_program_id,
    upgrade_contract_body, upgrade_contract_ix_data, GUARDIAN_COUNT, GUARDIAN_SET_INDEX,
    NOREPLAY_PROGRAM_ID, QUORUM,
};
use crate::harness::{
    deploy_programs, fund, rpc_client, send, send_expect_error, set_account, so_path,
    start_surfpool, ProgramImage, SurfpoolOptions,
};

/// Governance sequence consumed by the upgrade VAA.
const SEQUENCE: u64 = 1;

/// Buffer address carried in the VAA payload as `new_contract`.
const BUFFER: Pubkey = Pubkey::new_from_array([0xBF; 32]);

const BUFFER_LAMPORTS: u64 = 5_000_000_000;
const PAYER_LAMPORTS: u64 = 20_000_000_000;
const UPGRADE_CU_LIMIT: u32 = 1_000_000;

fn derive_program_data(program_id: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(&[program_id.as_ref()], &loader_v3_id()).0
}

/// Step 1 records the surfpool pid plus RPC URL here. Step 2 reads it; so does `e2e-upgrade-stop`.
fn state_file() -> PathBuf {
    so_path("global_accountant")
        .parent()
        .and_then(|p| p.parent())
        .expect("target dir from .so path")
        .join("e2e-upgrade-contract.json")
}

/// Loader header of a program, buffer, or program-data account. `bincode` stops at
/// the header and skips the trailing ELF bytes.
fn loader_state(account: &Account) -> UpgradeableLoaderState {
    bincode::deserialize(&account.data).expect("upgradeable loader state")
}

/// Serialized `header ‖ payload`, the layout of buffer and program-data accounts.
fn loader_account(header: &UpgradeableLoaderState, payload: &[u8]) -> Vec<u8> {
    let mut data = bincode::serialize(header).expect("serialize loader state");
    data.extend_from_slice(payload);
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
        let _ = std::process::Command::new("kill")
            .arg(pid.to_string())
            .status();
    }
    let _ = std::fs::remove_file(state_file());
}

/// Step 1: spin up surfpool and stage everything the upgrade VAA needs.
#[test]
#[ignore = "starts a long-lived surfpool; run via `just e2e-upgrade-deploy`"]
fn deploy_upgradeable_accountant() {
    kill_previous_instance();

    let guard = start_surfpool(SurfpoolOptions::offline_detached("ga-surfpool-upgrade"));
    let rpc = guard.rpc_client();

    let accountant = ProgramImage::accountant();
    let program_id = accountant.program_id;
    let elf = accountant.elf.clone();
    deploy_programs(
        &rpc,
        &[
            accountant,
            ProgramImage::noreplay(),
            ProgramImage::verify_vaa_shim(),
        ],
    );

    // Move the deployed program's upgrade authority to the `[b"upgrade"]` PDA, the
    // on-chain equivalent of `solana program set-upgrade-authority`.
    let (upgrade_authority, _) = derive_upgrade_authority(&program_id);
    let program = rpc.get_account(&program_id).expect("program account");
    assert_eq!(program.owner, loader_v3_id(), "program owner");
    let UpgradeableLoaderState::Program {
        programdata_address,
    } = loader_state(&program)
    else {
        panic!("program account is not loader state Program");
    };
    assert_eq!(
        programdata_address,
        derive_program_data(&program_id),
        "program points at the canonical program-data address"
    );
    let program_data = rpc
        .get_account(&programdata_address)
        .expect("program-data account");
    let UpgradeableLoaderState::ProgramData { slot, .. } = loader_state(&program_data) else {
        panic!("program-data account is not loader state ProgramData");
    };
    let metadata_len = UpgradeableLoaderState::size_of_programdata_metadata();
    set_account(
        &rpc,
        &programdata_address,
        &Account {
            data: loader_account(
                &UpgradeableLoaderState::ProgramData {
                    slot,
                    upgrade_authority_address: Some(upgrade_authority),
                },
                &program_data.data[metadata_len..],
            ),
            ..program_data
        },
    );
    eprintln!(
        "[upgrade-e2e] program={program_id} program_data={programdata_address} \
         upgrade_authority={upgrade_authority}"
    );

    // Guardian set the shim verifies against: test guardians at the Core Bridge PDA.
    let guardians = make_guardians(GUARDIAN_COUNT, 0x42);
    let (guardian_set, _) = derive_guardian_set_pda(GUARDIAN_SET_INDEX, &core_bridge_program_id());
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

    // Buffer holding the replacement image, authority = upgrade PDA.
    set_account(
        &rpc,
        &BUFFER,
        &Account {
            lamports: BUFFER_LAMPORTS,
            data: loader_account(
                &UpgradeableLoaderState::Buffer {
                    authority_address: Some(upgrade_authority),
                },
                &elf,
            ),
            owner: loader_v3_id(),
            executable: false,
            rent_epoch: 0,
        },
    );
    eprintln!(
        "[upgrade-e2e] buffer={BUFFER} elf={} bytes, authority={upgrade_authority}",
        elf.len()
    );

    let state = serde_json::json!({
        "rpc_url": guard.rpc_url(),
        "surfpool_pid": guard.pid(),
    });
    std::fs::write(state_file(), state.to_string()).expect("write state file");
    eprintln!(
        "[upgrade-e2e] surfpool pid={} rpc={}; state at {}",
        guard.pid(),
        guard.rpc_url(),
        state_file().display()
    );
    eprintln!("[upgrade-e2e] deploy step complete; run `just e2e-upgrade-submit` next");
    guard.detach();
}

/// Step 2: sign the governance VAA, submit `upgrade_contract`, check the upgrade.
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
    let rpc_url = state["rpc_url"].as_str().expect("rpc_url");
    let rpc = rpc_client(rpc_url);
    rpc.get_health().unwrap_or_else(|e| {
        panic!("surfpool at {rpc_url} not healthy: {e}. Run `just e2e-upgrade-deploy` first.")
    });

    let program_id = ProgramImage::accountant().program_id;
    let program_data = derive_program_data(&program_id);
    let (upgrade_authority, _) = derive_upgrade_authority(&program_id);
    let noreplay_authority = noreplay_authority_pda(&program_id);
    let (bucket, _) = derive_bucket_pda(
        &noreplay_authority,
        SOLANA_CHAIN_ID,
        &GOVERNANCE_EMITTER,
        SEQUENCE,
    );
    let (guardian_set, guardian_set_bump) =
        derive_guardian_set_pda(GUARDIAN_SET_INDEX, &core_bridge_program_id());

    if let Ok(existing) = rpc.get_account(&bucket) {
        let marked =
            global_accountant_definitions::NoReplayBitmapAccount::from_bytes(&existing.data)
                .is_some_and(|b| b.is_marked(SEQUENCE));
        assert!(
            !marked,
            "sequence {SEQUENCE} already consumed on this surfpool; \
             re-run `just e2e-upgrade-deploy` for a fresh instance"
        );
    }

    let payer = Keypair::new();
    fund(&rpc, &payer.pubkey(), PAYER_LAMPORTS);

    let body = upgrade_body();
    let digest = double_keccak256(&body);
    let guardians = make_guardians(GUARDIAN_COUNT, 0x42);
    let guardian_signatures = Keypair::new();
    send(
        &rpc,
        "post_signatures",
        &[post_signatures_ix(
            &payer.pubkey(),
            &guardian_signatures.pubkey(),
            GUARDIAN_SET_INDEX,
            QUORUM,
            &signature_block(&signatures_for(&guardians, &digest, QUORUM)),
        )],
        &[&payer, &guardian_signatures],
    );

    let pd_before = rpc.get_account(&program_data).expect("program-data before");
    let UpgradeableLoaderState::ProgramData {
        slot: slot_before, ..
    } = loader_state(&pd_before)
    else {
        panic!("program-data before is not loader state ProgramData");
    };
    let spill = Pubkey::new_unique();

    let upgrade_ix = || Instruction {
        program_id,
        accounts: vec![
            AccountMeta::new(payer.pubkey(), true),
            AccountMeta::new_readonly(shim_program_id(), false),
            AccountMeta::new_readonly(guardian_set, false),
            AccountMeta::new_readonly(guardian_signatures.pubkey(), false),
            AccountMeta::new(bucket, false),
            AccountMeta::new_readonly(NOREPLAY_PROGRAM_ID, false),
            AccountMeta::new_readonly(noreplay_authority, false),
            AccountMeta::new_readonly(system_program_id(), false),
            AccountMeta::new_readonly(upgrade_authority, false),
            AccountMeta::new(spill, false),
            AccountMeta::new(BUFFER, false),
            AccountMeta::new(program_data, false),
            AccountMeta::new(program_id, false),
            AccountMeta::new_readonly(rent_sysvar_id(), false),
            AccountMeta::new_readonly(clock_sysvar_id(), false),
            AccountMeta::new_readonly(loader_v3_id(), false),
        ],
        data: upgrade_contract_ix_data(guardian_set_bump, &body),
    };
    send(
        &rpc,
        "upgrade_contract",
        &[set_compute_unit_limit_ix(UPGRADE_CU_LIMIT), upgrade_ix()],
        &[&payer],
    );

    let pd_after = rpc.get_account(&program_data).expect("program-data after");
    let UpgradeableLoaderState::ProgramData {
        slot: slot_after, ..
    } = loader_state(&pd_after)
    else {
        panic!("program-data after is not loader state ProgramData");
    };
    assert!(
        slot_after > slot_before,
        "program-data slot advances: {slot_before} -> {slot_after}"
    );
    let elf = ProgramImage::accountant().elf;
    let metadata_len = UpgradeableLoaderState::size_of_programdata_metadata();
    assert_eq!(
        &pd_after.data[metadata_len..metadata_len + elf.len()],
        &elf[..],
        "program-data carries the buffer image"
    );

    let buffer_after = rpc.get_account(&BUFFER);
    let buffer_closed = match &buffer_after {
        Ok(acct) => {
            acct.lamports == 0
                || acct.data.len() < UpgradeableLoaderState::size_of_buffer_metadata()
        }
        Err(_) => true,
    };
    assert!(
        buffer_closed,
        "buffer closed by the loader: {buffer_after:?}"
    );
    let spill_after = rpc.get_account(&spill).expect("spill after");
    assert!(
        spill_after.lamports > 0,
        "spill received the buffer lamports"
    );
    eprintln!(
        "[upgrade-e2e] slot {slot_before} -> {slot_after}; spill received {} lamports",
        spill_after.lamports
    );

    let bucket_after = rpc.get_account(&bucket).expect("noreplay bucket after");
    assert_bucket_marked(&bucket_after, SEQUENCE);

    // Replay of the same VAA must reject with AlreadyAccounted. This second invocation
    // also proves the upgraded image executes.
    send_expect_error(
        &rpc,
        "upgrade_contract replay",
        &[set_compute_unit_limit_ix(UPGRADE_CU_LIMIT), upgrade_ix()],
        &[&payer],
        GlobalAccountantError::AlreadyAccounted,
    );
}
