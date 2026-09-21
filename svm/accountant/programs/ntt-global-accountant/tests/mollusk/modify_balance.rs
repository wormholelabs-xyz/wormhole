//! The shared `modify_balance` handler under the NTT accountant module.

use accountant_operational_core::accounts::balance;
use accountant_operational_core::instructions::modify_balance::derive_modify_balance_pda;
use global_accountant_definitions::{
    GlobalAccountantError, GovernanceModule, ModificationKind, Uint256,
    ACCOUNTANT_GOVERNANCE_MODULE, GOVERNANCE_EMITTER, MODIFY_BALANCE_ACTION,
    NTT_ACCOUNTANT_GOVERNANCE_MODULE, SOLANA_CHAIN_ID,
};
use mollusk_svm::program::keyed_account_for_system_program;
use mollusk_svm::result::InstructionResult;
use mollusk_svm::Mollusk;
use solana_instruction::AccountMeta;
use solana_pubkey::Pubkey;

use crate::common::*;

const HUB_TOKEN: [u8; 32] = [0x77u8; 32];
const REASON: [u8; 32] = *b"audit-log: post-incident credit ";

struct Modification {
    vaa: SignedVaa,
    balance_pda: Pubkey,
    modify_balance_pda: Pubkey,
}

impl Modification {
    fn add(module: GovernanceModule, payload_sequence: u64, amount: u128) -> Self {
        let header = governance_header(module, MODIFY_BALANCE_ACTION, SOLANA_CHAIN_ID);
        let body = modify_balance_body(
            SOLANA_CHAIN_ID,
            GOVERNANCE_EMITTER,
            0x10,
            header,
            payload_sequence,
            ETHEREUM,
            ETHEREUM,
            HUB_TOKEN,
            ModificationKind::Add as u8,
            Uint256::from_u128(amount),
            REASON,
        );
        Self {
            vaa: SignedVaa::new(body),
            balance_pda: balance::derive_pda(&program_id(), ETHEREUM, ETHEREUM, &HUB_TOKEN).0,
            modify_balance_pda: derive_modify_balance_pda(&program_id(), payload_sequence).0,
        }
    }

    fn submit(&self, mollusk: &Mollusk) -> InstructionResult {
        self.vaa.submit(
            mollusk,
            program_id(),
            &modify_balance_ix_data(self.vaa.guardian_set_bump, &self.vaa.body),
            vec![
                AccountMeta::new(self.balance_pda, false),
                AccountMeta::new_readonly(system_program_id(), false),
                AccountMeta::new(self.modify_balance_pda, false),
            ],
            vec![
                (self.balance_pda, uninitialised_pda_account()),
                keyed_account_for_system_program(),
                (self.modify_balance_pda, uninitialised_pda_account()),
            ],
        )
    }
}

#[test]
fn ntt_module_applies_and_wtt_module_is_rejected() {
    let mollusk = mollusk();

    let add = Modification::add(NTT_ACCOUNTANT_GOVERNANCE_MODULE, 200, 1_000_000);
    let result = add.submit(&mollusk);
    assert_success(&result, "add on uninitialised balance");
    assert_balance_for(
        &program_id(),
        &result.resulting_accounts,
        &add.balance_pda,
        Uint256::from_u128(1_000_000),
    );

    let wtt = Modification::add(ACCOUNTANT_GOVERNANCE_MODULE, 201, 1);
    assert_error(
        &wtt.submit(&mollusk),
        GlobalAccountantError::InvalidGovernanceModule as u64,
        "wtt accountant module",
    );
}
