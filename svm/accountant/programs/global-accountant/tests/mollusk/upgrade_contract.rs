use accountant_operational_core::cpi::loader::derive_upgrade_authority;
use accountant_operational_core::cpi::noreplay::derive_bucket_pda;
use global_accountant_definitions::{
    GlobalAccountantError, GovernanceHeader, ACCOUNTANT_GOVERNANCE_MODULE, GOVERNANCE_EMITTER,
    MODIFY_BALANCE_ACTION, NOREPLAY_AUTHORITY_SEED_PREFIX, SOLANA_CHAIN_ID,
    TOKEN_BRIDGE_GOVERNANCE_MODULE, UPGRADE_CONTRACT_ACTION,
};
use mollusk_svm::program::keyed_account_for_system_program;
use mollusk_svm::result::InstructionResult;
use mollusk_svm::Mollusk;
use solana_account::Account;
use solana_instruction::{AccountMeta, Instruction};
use solana_pubkey::Pubkey;

use crate::common::*;

const NEW_CONTRACT: [u8; 32] = [0xC4; 32];

fn loader_id() -> Pubkey {
    Pubkey::from_str_const("BPFLoaderUpgradeab1e11111111111111111111111")
}

fn rent_sysvar_id() -> Pubkey {
    Pubkey::from_str_const("SysvarRent111111111111111111111111111111111")
}

fn clock_sysvar_id() -> Pubkey {
    Pubkey::from_str_const("SysvarC1ock11111111111111111111111111111111")
}

fn solana_target() -> GovernanceHeader {
    governance_header(
        ACCOUNTANT_GOVERNANCE_MODULE,
        UPGRADE_CONTRACT_ACTION,
        SOLANA_CHAIN_ID,
    )
}

fn program_elf() -> Vec<u8> {
    let dir = std::env::var("SBF_OUT_DIR").expect("SBF_OUT_DIR");
    std::fs::read(format!("{dir}/global_accountant.so")).expect("global_accountant.so")
}

fn buffer_account(authority: &Pubkey, elf: &[u8]) -> Account {
    let mut data = Vec::with_capacity(37 + elf.len());
    data.extend_from_slice(&1u32.to_le_bytes());
    data.push(1);
    data.extend_from_slice(authority.as_ref());
    data.extend_from_slice(elf);
    Account {
        lamports: 10_000_000_000,
        data,
        owner: loader_id(),
        executable: false,
        rent_epoch: 0,
    }
}

fn program_data_account(authority: &Pubkey, elf_len: usize) -> Account {
    let mut data = vec![0u8; 45 + elf_len + 1024];
    data[..4].copy_from_slice(&3u32.to_le_bytes());
    data[4..12].copy_from_slice(&1u64.to_le_bytes());
    data[12] = 1;
    data[13..45].copy_from_slice(authority.as_ref());
    Account {
        lamports: 10_000_000_000,
        data,
        owner: loader_id(),
        executable: false,
        rent_epoch: 0,
    }
}

fn program_account(program_data: &Pubkey) -> Account {
    let mut data = vec![0u8; 36];
    data[..4].copy_from_slice(&2u32.to_le_bytes());
    data[4..36].copy_from_slice(program_data.as_ref());
    Account {
        lamports: 1_000_000_000,
        data,
        owner: loader_id(),
        executable: true,
        rent_epoch: 0,
    }
}

#[derive(Clone)]
struct Upgrade {
    sequence: u64,
    body: Vec<u8>,
    guardian_set_bump: u8,
    payer: Pubkey,
    guardian_set: Pubkey,
    guardian_signatures: Pubkey,
    noreplay_bucket: Pubkey,
    noreplay_authority: Pubkey,
    upgrade_authority: Pubkey,
    spill: Pubkey,
    buffer: Pubkey,
    program_data: Pubkey,
    buffer_state: Account,
    program_data_state: Account,
    guardians: Vec<Guardian>,
}

impl Upgrade {
    fn new(sequence: u64) -> Self {
        Self::with_body(
            sequence,
            upgrade_contract_body(
                SOLANA_CHAIN_ID,
                GOVERNANCE_EMITTER,
                sequence,
                solana_target(),
                NEW_CONTRACT,
            ),
        )
    }

    fn with_body(sequence: u64, body: Vec<u8>) -> Self {
        let (noreplay_authority, _) =
            Pubkey::find_program_address(&[NOREPLAY_AUTHORITY_SEED_PREFIX], &program_id());
        let (guardian_set, guardian_set_bump) =
            derive_guardian_set_pda(GUARDIAN_SET_INDEX, &core_bridge_program_id());
        let (upgrade_authority, _) = derive_upgrade_authority(&program_id());
        let elf = program_elf();
        Self {
            sequence,
            body,
            guardian_set_bump,
            payer: SUBMITTER,
            guardian_set,
            guardian_signatures: GUARDIAN_SIGNATURES,
            noreplay_bucket: derive_bucket_pda(
                &noreplay_authority,
                SOLANA_CHAIN_ID,
                &GOVERNANCE_EMITTER,
                sequence,
            )
            .0,
            noreplay_authority,
            upgrade_authority,
            spill: Pubkey::new_unique(),
            buffer: Pubkey::new_from_array(NEW_CONTRACT),
            program_data: Pubkey::find_program_address(&[program_id().as_ref()], &loader_id()).0,
            buffer_state: buffer_account(&upgrade_authority, &elf),
            program_data_state: program_data_account(&upgrade_authority, elf.len()),
            guardians: make_guardians(GUARDIAN_COUNT, 0x42),
        }
    }

    fn account_metas(&self) -> Vec<AccountMeta> {
        vec![
            AccountMeta::new(self.payer, true),
            AccountMeta::new_readonly(shim_program_id(), false),
            AccountMeta::new_readonly(self.guardian_set, false),
            AccountMeta::new_readonly(self.guardian_signatures, false),
            AccountMeta::new(self.noreplay_bucket, false),
            AccountMeta::new_readonly(noreplay_program_id(), false),
            AccountMeta::new_readonly(self.noreplay_authority, false),
            AccountMeta::new_readonly(system_program_id(), false),
            AccountMeta::new_readonly(self.upgrade_authority, false),
            AccountMeta::new(self.spill, false),
            AccountMeta::new(self.buffer, false),
            AccountMeta::new(self.program_data, false),
            AccountMeta::new(program_id(), false),
            AccountMeta::new_readonly(rent_sysvar_id(), false),
            AccountMeta::new_readonly(clock_sysvar_id(), false),
            AccountMeta::new_readonly(loader_id(), false),
        ]
    }

    fn accounts(&self, mollusk: &Mollusk, bucket: Account) -> Vec<(Pubkey, Account)> {
        let digest = double_keccak256(&self.body);
        let signatures: Vec<(u8, [u8; 65])> = (0..QUORUM)
            .map(|i| (i, sign_digest(&self.guardians[i as usize], &digest)))
            .collect();
        let keys: Vec<[u8; GUARDIAN_PUBKEY_LENGTH]> =
            self.guardians.iter().map(|g| g.eth_address).collect();
        vec![
            (self.payer, system_owned_account(50_000_000_000)),
            keyed_account_for_verify_vaa_shim_program(),
            (
                self.guardian_set,
                guardian_set_account(GUARDIAN_SET_INDEX, &keys, 0, 0, &core_bridge_program_id()),
            ),
            (
                self.guardian_signatures,
                guardian_signatures_account(
                    GUARDIAN_SET_INDEX,
                    &self.payer,
                    &signatures,
                    &shim_program_id(),
                ),
            ),
            (self.noreplay_bucket, bucket),
            keyed_account_for_noreplay_program(),
            (self.noreplay_authority, system_owned_account(0)),
            keyed_account_for_system_program(),
            (self.upgrade_authority, system_owned_account(0)),
            (self.spill, system_owned_account(0)),
            (self.buffer, self.buffer_state.clone()),
            (self.program_data, self.program_data_state.clone()),
            (program_id(), program_account(&self.program_data)),
            mollusk.sysvars.keyed_account_for_rent_sysvar(),
            mollusk.sysvars.keyed_account_for_clock_sysvar(),
            (
                loader_id(),
                Account {
                    lamports: 1,
                    data: vec![],
                    owner: Pubkey::from_str_const("NativeLoader1111111111111111111111111111111"),
                    executable: true,
                    rent_epoch: 0,
                },
            ),
        ]
    }

    fn submit(&self, mollusk: &Mollusk, accounts: Vec<(Pubkey, Account)>) -> InstructionResult {
        let ix = Instruction::new_with_bytes(
            program_id(),
            &upgrade_contract_ix_data(self.guardian_set_bump, &self.body),
            self.account_metas(),
        );
        mollusk.process_instruction(&ix, &accounts)
    }
}

#[test]
fn upgrade_replaces_program_data_and_marks_noreplay() {
    let mollusk = mollusk();
    let upgrade = Upgrade::new(20);
    let accounts = upgrade.accounts(&mollusk, noreplay_bucket_unmarked());
    let result = upgrade.submit(&mollusk, accounts);
    assert_success(&result, "upgrade");

    let program_data = find_account(&result.resulting_accounts, &upgrade.program_data);
    assert_eq!(
        u32::from_le_bytes(program_data.data[..4].try_into().unwrap()),
        3
    );
    assert_eq!(
        u64::from_le_bytes(program_data.data[4..12].try_into().unwrap()),
        0,
        "program data slot moves to the upgrade slot"
    );
    let elf = program_elf();
    assert_eq!(&program_data.data[45..45 + elf.len()], &elf[..]);
    assert_bucket_marked(
        find_account(&result.resulting_accounts, &upgrade.noreplay_bucket),
        upgrade.sequence,
    );
    let spill = find_account(&result.resulting_accounts, &upgrade.spill);
    assert!(spill.lamports > 0, "spill receives the freed lamports");
}

#[test]
fn rejects() {
    let mollusk = mollusk();

    let body = |header: GovernanceHeader, emitter_chain: u16, sequence: u64| {
        upgrade_contract_body(
            emitter_chain,
            GOVERNANCE_EMITTER,
            sequence,
            header,
            NEW_CONTRACT,
        )
    };
    let mut wrong_module = solana_target();
    wrong_module.module = TOKEN_BRIDGE_GOVERNANCE_MODULE;
    let mut modify_action = solana_target();
    modify_action.action = MODIFY_BALANCE_ACTION;
    let mut any_target = solana_target();
    any_target.target_chain = [0; 2];

    let mut wrong_authority = Upgrade::new(25);
    wrong_authority.upgrade_authority = Pubkey::new_unique();
    let mut wrong_buffer = Upgrade::new(26);
    wrong_buffer.buffer = Pubkey::new_unique();
    let mut wrong_program_data = Upgrade::new(27);
    wrong_program_data.program_data = Pubkey::new_unique();
    let mut truncated = Upgrade::new(28);
    truncated.body.pop();

    let cases: [(&str, Upgrade, Account, u64); 9] = [
        (
            "wrong module",
            Upgrade::with_body(21, body(wrong_module, SOLANA_CHAIN_ID, 21)),
            noreplay_bucket_unmarked(),
            GlobalAccountantError::InvalidGovernanceModule as u64,
        ),
        (
            "modify_balance action",
            Upgrade::with_body(22, body(modify_action, SOLANA_CHAIN_ID, 22)),
            noreplay_bucket_unmarked(),
            GlobalAccountantError::InvalidGovernanceAction as u64,
        ),
        (
            "any target chain",
            Upgrade::with_body(23, body(any_target, SOLANA_CHAIN_ID, 23)),
            noreplay_bucket_unmarked(),
            GlobalAccountantError::GovernanceChainMismatch as u64,
        ),
        (
            "wrong governance emitter chain",
            Upgrade::with_body(24, body(solana_target(), 2, 24)),
            noreplay_bucket_unmarked(),
            GlobalAccountantError::InvalidGovernanceEmitter as u64,
        ),
        (
            "pre-marked noreplay",
            Upgrade::new(30),
            noreplay_bucket_marked(30),
            GlobalAccountantError::AlreadyAccounted as u64,
        ),
        (
            "wrong upgrade authority pda",
            wrong_authority,
            noreplay_bucket_unmarked(),
            GlobalAccountantError::InvalidPda as u64,
        ),
        (
            "buffer not new contract",
            wrong_buffer,
            noreplay_bucket_unmarked(),
            GlobalAccountantError::InvalidPda as u64,
        ),
        (
            "wrong program data address",
            wrong_program_data,
            noreplay_bucket_unmarked(),
            GlobalAccountantError::InvalidPda as u64,
        ),
        (
            "body one byte short",
            truncated,
            noreplay_bucket_unmarked(),
            GlobalAccountantError::InvalidInstructionData as u64,
        ),
    ];

    for (label, upgrade, bucket, expected) in cases {
        let bucket_before = bucket.data.clone();
        let accounts = upgrade.accounts(&mollusk, bucket);
        let result = upgrade.submit(&mollusk, accounts);
        assert_error(&result, expected, label);
        assert_eq!(
            find_account(&result.resulting_accounts, &upgrade.noreplay_bucket).data,
            bucket_before,
            "{label}: replay slot must be unchanged"
        );
    }
}
