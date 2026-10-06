//! `submit_observations` and `submit_vaas` run only as the first top-level instruction. An
//! earlier instruction can fill the transaction log and drop the commit log.

use global_accountant_definitions::GlobalAccountantError;
use mollusk_svm::program::create_program_account_loader_v3;
use solana_account::Account;
use solana_instruction::{AccountMeta, Instruction};
use solana_pubkey::Pubkey;

use crate::common::*;

/// Label, accountant instruction, initial accounts.
type Case = (&'static str, Instruction, Vec<(Pubkey, Account)>);

#[test]
fn rejects_second_instruction() {
    let mollusk = mollusk();
    let vaa = VaaScenario::transfer(Transfer::new(0xD0, ETHEREUM, SOLANA, 100));
    let obs = ObsScenario::transfer(
        GUARDIAN_SET_INDEX,
        0xD1,
        Transfer::new(0, ETHEREUM, SOLANA, 100),
    );
    let cases: [Case; 2] = [
        (
            "submit_vaas",
            Instruction::new_with_bytes(
                program_id(),
                &submit_vaas_ix_data(vaa.vaa.guardian_set_bump, &vaa.vaa.body),
                vaa.account_metas(),
            ),
            vaa.accounts(),
        ),
        (
            "submit_observations",
            Instruction::new_with_bytes(program_id(), &obs.ix_data(0), obs.account_metas()),
            obs.initial_accounts(),
        ),
    ];
    for (label, ix, accounts) in cases {
        let result = process_as_second_instruction(&mollusk, &ix, &accounts);
        assert_tx_error(
            &result,
            1,
            GlobalAccountantError::InstructionNotFirst as u64,
            label,
        );
    }
}

#[test]
fn submit_vaas_rejects_cpi_invocation() {
    let forwarder = Pubkey::new_from_array([0xF0u8; 32]);
    let mut mollusk = mollusk();
    mollusk.add_program(&forwarder, "test_cpi_forwarder");
    let scenario = VaaScenario::transfer(Transfer::new(0xD2, ETHEREUM, SOLANA, 100));

    let mut metas = vec![AccountMeta::new_readonly(program_id(), false)];
    metas.extend(scenario.account_metas());
    let mut accounts = scenario.accounts();
    accounts.push((
        program_id(),
        create_program_account_loader_v3(&program_id()),
    ));
    let ix = Instruction::new_with_bytes(
        forwarder,
        &submit_vaas_ix_data(scenario.vaa.guardian_set_bump, &scenario.vaa.body),
        metas,
    );
    let result = mollusk.process_instruction(&ix, &accounts);

    assert_error(
        &result,
        GlobalAccountantError::CpiInvocation as u64,
        "submit_vaas through CPI",
    );
}
