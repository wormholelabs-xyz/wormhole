//! The shared `upgrade_contract` handler under the NTT accountant module.

use accountant_operational_core::cpi::loader::derive_upgrade_authority;
use accountant_operational_core::cpi::noreplay::derive_bucket_pda;
use global_accountant_definitions::{
    GlobalAccountantError, GovernanceModule, ACCOUNTANT_GOVERNANCE_MODULE, GOVERNANCE_EMITTER,
    NTT_ACCOUNTANT_GOVERNANCE_MODULE, SOLANA_CHAIN_ID, UPGRADE_CONTRACT_ACTION,
};
use mollusk_svm::program::keyed_account_for_system_program;
use mollusk_svm::result::InstructionResult;
use mollusk_svm::Mollusk;
use solana_instruction::AccountMeta;
use solana_pubkey::Pubkey;

use crate::common::*;

const NEW_CONTRACT: [u8; 32] = [0xC4; 32];

struct Upgrade {
    vaa: GovernanceVaa,
    sequence: u64,
    noreplay_bucket: Pubkey,
    noreplay_authority: Pubkey,
    upgrade_authority: Pubkey,
    spill: Pubkey,
    program_data: Pubkey,
    elf: Vec<u8>,
}

impl Upgrade {
    fn new(module: GovernanceModule, sequence: u64) -> Self {
        let header = governance_header(module, UPGRADE_CONTRACT_ACTION, SOLANA_CHAIN_ID);
        let body = upgrade_contract_body(
            SOLANA_CHAIN_ID,
            GOVERNANCE_EMITTER,
            sequence,
            header,
            NEW_CONTRACT,
        );
        let noreplay_authority = noreplay_authority_pda(&program_id());
        Self {
            vaa: GovernanceVaa::new(body),
            sequence,
            noreplay_bucket: derive_bucket_pda(
                &noreplay_authority,
                SOLANA_CHAIN_ID,
                &GOVERNANCE_EMITTER,
                sequence,
            )
            .0,
            noreplay_authority,
            upgrade_authority: derive_upgrade_authority(&program_id()).0,
            spill: Pubkey::new_unique(),
            program_data: Pubkey::find_program_address(&[program_id().as_ref()], &loader_v3_id()).0,
            elf: deployed_elf(PROGRAM_NAME),
        }
    }

    fn submit(&self, mollusk: &Mollusk) -> InstructionResult {
        let buffer = Pubkey::new_from_array(NEW_CONTRACT);
        self.vaa.submit(
            mollusk,
            program_id(),
            &upgrade_contract_ix_data(self.vaa.guardian_set_bump, &self.vaa.body),
            vec![
                AccountMeta::new(self.noreplay_bucket, false),
                AccountMeta::new_readonly(noreplay_program_id(), false),
                AccountMeta::new_readonly(self.noreplay_authority, false),
                AccountMeta::new_readonly(system_program_id(), false),
                AccountMeta::new_readonly(self.upgrade_authority, false),
                AccountMeta::new(self.spill, false),
                AccountMeta::new(buffer, false),
                AccountMeta::new(self.program_data, false),
                AccountMeta::new(program_id(), false),
                AccountMeta::new_readonly(rent_sysvar_id(), false),
                AccountMeta::new_readonly(clock_sysvar_id(), false),
                AccountMeta::new_readonly(loader_v3_id(), false),
            ],
            vec![
                (self.noreplay_bucket, noreplay_bucket_unmarked()),
                keyed_account_for_noreplay_program(),
                (self.noreplay_authority, system_owned_account(0)),
                keyed_account_for_system_program(),
                (self.upgrade_authority, system_owned_account(0)),
                (self.spill, system_owned_account(0)),
                (
                    buffer,
                    upgradeable_buffer_account(&self.upgrade_authority, &self.elf),
                ),
                (
                    self.program_data,
                    upgradeable_program_data_account(&self.upgrade_authority, self.elf.len()),
                ),
                (
                    program_id(),
                    upgradeable_program_account(&self.program_data),
                ),
                mollusk.sysvars.keyed_account_for_rent_sysvar(),
                mollusk.sysvars.keyed_account_for_clock_sysvar(),
                keyed_account_for_loader_v3(),
            ],
        )
    }
}

#[test]
fn wtt_module_is_rejected_then_ntt_module_upgrades() {
    let mollusk = mollusk();

    // First: a program upgraded in a slot cannot be invoked again in that slot.
    let wtt = Upgrade::new(ACCOUNTANT_GOVERNANCE_MODULE, 21);
    assert_error(
        &wtt.submit(&mollusk),
        GlobalAccountantError::InvalidGovernanceModule as u64,
        "wtt accountant module",
    );

    let upgrade = Upgrade::new(NTT_ACCOUNTANT_GOVERNANCE_MODULE, 20);
    let result = upgrade.submit(&mollusk);
    assert_success(&result, "upgrade");
    let program_data = find_account(&result.resulting_accounts, &upgrade.program_data);
    assert_eq!(
        &program_data.data[45..45 + upgrade.elf.len()],
        &upgrade.elf[..]
    );
    assert_bucket_marked(
        find_account(&result.resulting_accounts, &upgrade.noreplay_bucket),
        upgrade.sequence,
    );
}
