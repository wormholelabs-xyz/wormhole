//! The shared `register_chain` handler under the Standard Relayer module. Registrations are
//! `ChainRegistrationLayout` PDAs, so `chain_registration::verify` serves the transfer paths.

use accountant_operational_core::accounts::chain_registration;
use accountant_operational_core::instructions::register_chain::derive_register_chain_pda;
use global_accountant_definitions::{
    ChainRegistrationLayout, GlobalAccountantError, GovernanceModule, RegisterChainLayout,
    GOVERNANCE_EMITTER, REGISTER_CHAIN_ACTION, RELAYER_GOVERNANCE_MODULE, SOLANA_CHAIN_ID,
    TOKEN_BRIDGE_GOVERNANCE_MODULE,
};
use mollusk_svm::program::keyed_account_for_system_program;
use mollusk_svm::result::InstructionResult;
use mollusk_svm::Mollusk;
use solana_account::Account;
use solana_instruction::AccountMeta;
use solana_pubkey::Pubkey;

use crate::common::*;

struct Registration {
    vaa: SignedVaa,
    registration_pda: Pubkey,
    register_chain_pda: Pubkey,
}

impl Registration {
    fn new(module: GovernanceModule, sequence: u64, emitter: [u8; 32]) -> Self {
        let header = governance_header(module, REGISTER_CHAIN_ACTION, 0);
        let body = register_chain_body(
            SOLANA_CHAIN_ID,
            GOVERNANCE_EMITTER,
            sequence,
            header,
            ETHEREUM,
            emitter,
        );
        Self {
            vaa: SignedVaa::new(body),
            registration_pda: chain_registration::derive_pda(&program_id(), ETHEREUM).0,
            register_chain_pda: derive_register_chain_pda(&program_id(), sequence).0,
        }
    }

    fn submit(
        &self,
        mollusk: &Mollusk,
        registration: Account,
        record: Account,
    ) -> InstructionResult {
        self.vaa.submit(
            mollusk,
            program_id(),
            &register_relayer_chain_ix_data(self.vaa.guardian_set_bump, &self.vaa.body),
            vec![
                AccountMeta::new(self.registration_pda, false),
                AccountMeta::new_readonly(system_program_id(), false),
                AccountMeta::new(self.register_chain_pda, false),
            ],
            vec![
                (self.registration_pda, registration),
                keyed_account_for_system_program(),
                (self.register_chain_pda, record),
            ],
        )
    }
}

#[test]
fn relayer_module_registers_and_wtt_module_is_rejected() {
    let mollusk = mollusk();

    let relayer = Registration::new(RELAYER_GOVERNANCE_MODULE, 6, RELAYER);
    let result = relayer.submit(
        &mollusk,
        uninitialised_pda_account(),
        uninitialised_pda_account(),
    );
    assert_success(&result, "relayer registration");
    let registration = find_account(&result.resulting_accounts, &relayer.registration_pda);
    assert_eq!(registration.owner, program_id());
    assert_eq!(
        *bytemuck::from_bytes::<ChainRegistrationLayout>(&registration.data),
        ChainRegistrationLayout::new(ETHEREUM, RELAYER, 6)
    );
    let record = find_account(&result.resulting_accounts, &relayer.register_chain_pda);
    assert_eq!(
        *bytemuck::from_bytes::<RegisterChainLayout>(&record.data),
        RegisterChainLayout::new(ETHEREUM, RELAYER, 6)
    );

    let mut spoofed_registration = Registration::new(RELAYER_GOVERNANCE_MODULE, 8, RELAYER);
    spoofed_registration.registration_pda = Pubkey::new_unique();
    let cases: [(&str, Registration, Account, GlobalAccountantError); 3] = [
        (
            "token bridge module",
            Registration::new(TOKEN_BRIDGE_GOVERNANCE_MODULE, 7, RELAYER),
            uninitialised_pda_account(),
            GlobalAccountantError::InvalidGovernanceModule,
        ),
        (
            "non-canonical registration pda",
            spoofed_registration,
            uninitialised_pda_account(),
            GlobalAccountantError::InvalidPda,
        ),
        (
            "replayed sequence",
            Registration::new(RELAYER_GOVERNANCE_MODULE, 6, RELAYER),
            record.clone(),
            GlobalAccountantError::DuplicateRegisterChain,
        ),
    ];
    for (label, registration, record, expected) in cases {
        let result = registration.submit(&mollusk, uninitialised_pda_account(), record);
        assert_error(&result, expected as u64, label);
    }
}
