//! Two-step surfpool e2e for `upgrade_contract`, shared by both accountant programs.
//!
//! Step 1 ([`UpgradeE2e::deploy`]): start a detached offline surfpool, deploy the accountant
//! with its upgrade authority moved to the `[b"upgrade"]` PDA, deploy the NoReplay and
//! Verify-VAA-Shim programs, seed a test guardian set, and stage the buffer holding the new
//! program image. Surfpool keeps running after this exits.
//!
//! Step 2 ([`UpgradeE2e::submit`]): sign the `UpgradeContract` governance VAA with the test
//! guardians, post the signatures through the real shim, submit `upgrade_contract`, then
//! assert the loader rewrote the program-data account, the buffer closed into the spill,
//! NoReplay marked the sequence, and a replay of the same VAA rejects with `AlreadyAccounted`.
//!
//! The `just e2e-upgrade-*` recipes drive the steps and kill the instance.

use std::path::PathBuf;

use accountant_operational_core::cpi::loader::derive_upgrade_authority;
use accountant_operational_core::cpi::noreplay::derive_bucket_pda;
use global_accountant_definitions::{
    GlobalAccountantError, GovernanceModule, NoReplayBitmapAccount, GOVERNANCE_EMITTER,
    SOLANA_CHAIN_ID, UPGRADE_CONTRACT_ACTION,
};
use solana_account::Account;
use solana_instruction::{AccountMeta, Instruction};
use solana_keypair::Keypair;
use solana_loader_v3_interface::state::UpgradeableLoaderState;
use solana_pubkey::Pubkey;
use solana_signer::Signer;

use crate::accounts::{
    assert_bucket_marked, loader_account_data, loader_state, noreplay_authority_pda,
    program_data_address,
};
use crate::guardians::{derive_guardian_set_pda, make_guardians, signature_block};
use crate::ids::{
    clock_sysvar_id, core_bridge_program_id, loader_v3_id, rent_sysvar_id, shim_program_id,
    system_program_id, NOREPLAY_PROGRAM_ID,
};
use crate::ix::{
    double_keccak256, governance_header, post_signatures_ix, set_compute_unit_limit_ix,
    upgrade_contract_body,
};
use crate::scenario::{signatures_for, GUARDIAN_COUNT, GUARDIAN_SET_INDEX, QUORUM};
use crate::surfpool::{
    deploy_guardian_set, deploy_programs, fund, rpc_client, send, send_expect_error, set_account,
    so_path, start_surfpool, ProgramImage, SurfpoolOptions,
};
use crate::wire;

/// Governance sequence consumed by the upgrade VAA.
const SEQUENCE: u64 = 1;

/// Buffer address carried in the VAA payload as `new_contract`.
const BUFFER: Pubkey = Pubkey::new_from_array([0xBF; 32]);

const BUFFER_LAMPORTS: u64 = 5_000_000_000;
const PAYER_LAMPORTS: u64 = 20_000_000_000;
const UPGRADE_CU_LIMIT: u32 = 1_000_000;

/// One program's upgrade flow.
pub struct UpgradeE2e {
    /// The accountant under test, from the deploy dir.
    pub image: ProgramImage,
    /// Governance module the program's `upgrade_contract` accepts.
    pub module: GovernanceModule,
    /// The program's `UpgradeContract` discriminator byte.
    pub discriminator: u8,
    /// Scratch-dir prefix for the surfpool instance.
    pub scratch_prefix: &'static str,
    /// Cargo package name; keys the state file so both programs can run side by side.
    pub package: &'static str,
}

/// `target/e2e-upgrade-contract-<package>.json`: surfpool pid plus RPC URL from step 1.
pub fn upgrade_state_file(so_label: &str, package: &str) -> PathBuf {
    so_path(so_label)
        .parent()
        .and_then(|p| p.parent())
        .expect("target dir from .so path")
        .join(format!("e2e-upgrade-contract-{package}.json"))
}

impl UpgradeE2e {
    fn state_file(&self) -> PathBuf {
        upgrade_state_file(self.image.label, self.package)
    }

    fn upgrade_body(&self) -> Vec<u8> {
        upgrade_contract_body(
            SOLANA_CHAIN_ID,
            GOVERNANCE_EMITTER,
            SEQUENCE,
            governance_header(self.module, UPGRADE_CONTRACT_ACTION, SOLANA_CHAIN_ID),
            BUFFER.to_bytes(),
        )
    }

    /// Kill the surfpool recorded in the state file, so re-deploys replace instead of orphan.
    fn kill_previous_instance(&self) {
        let Ok(raw) = std::fs::read_to_string(self.state_file()) else {
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
        let _ = std::fs::remove_file(self.state_file());
    }

    /// Step 1: spin up surfpool and stage everything the upgrade VAA needs.
    pub fn deploy(self) {
        self.kill_previous_instance();
        let state_file = self.state_file();

        let guard = start_surfpool(SurfpoolOptions::offline_detached(self.scratch_prefix));
        let rpc = guard.rpc_client();

        let program_id = self.image.program_id;
        let elf = self.image.elf.clone();
        deploy_programs(
            &rpc,
            &[
                self.image,
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
            program_data_address(&program_id),
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
                data: loader_account_data(
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

        let guardians = make_guardians(GUARDIAN_COUNT, 0x42);
        deploy_guardian_set(&rpc, GUARDIAN_SET_INDEX, &guardians);

        // Buffer holding the replacement image, authority = upgrade PDA.
        set_account(
            &rpc,
            &BUFFER,
            &Account {
                lamports: BUFFER_LAMPORTS,
                data: loader_account_data(
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
        std::fs::write(&state_file, state.to_string()).expect("write state file");
        eprintln!(
            "[upgrade-e2e] surfpool pid={} rpc={}; state at {}",
            guard.pid(),
            guard.rpc_url(),
            state_file.display()
        );
        eprintln!("[upgrade-e2e] deploy step complete; run `just e2e-upgrade-submit` next");
        guard.detach();
    }

    /// Step 2: sign the governance VAA, submit `upgrade_contract`, check the upgrade.
    pub fn submit(self) {
        let state_file = self.state_file();
        let state_raw = std::fs::read_to_string(&state_file).unwrap_or_else(|e| {
            panic!(
                "read {}: {e}. Run `just e2e-upgrade-deploy` first.",
                state_file.display()
            )
        });
        let state: serde_json::Value = serde_json::from_str(&state_raw).expect("state JSON");
        let rpc_url = state["rpc_url"].as_str().expect("rpc_url");
        let rpc = rpc_client(rpc_url);
        rpc.get_health().unwrap_or_else(|e| {
            panic!("surfpool at {rpc_url} not healthy: {e}. Run `just e2e-upgrade-deploy` first.")
        });

        let program_id = self.image.program_id;
        let program_data = program_data_address(&program_id);
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
            let marked = NoReplayBitmapAccount::from_bytes(&existing.data)
                .is_some_and(|b| b.is_marked(SEQUENCE));
            assert!(
                !marked,
                "sequence {SEQUENCE} already consumed on this surfpool; \
                 re-run `just e2e-upgrade-deploy` for a fresh instance"
            );
        }

        let payer = Keypair::new();
        fund(&rpc, &payer.pubkey(), PAYER_LAMPORTS);

        let body = self.upgrade_body();
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
            data: wire::upgrade_contract(self.discriminator, guardian_set_bump, &body),
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
        let elf = &self.image.elf;
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
}
