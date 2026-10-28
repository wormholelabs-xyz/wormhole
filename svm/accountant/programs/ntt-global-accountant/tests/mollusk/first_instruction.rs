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

const SOLANA_HUB: (u16, [u8; 32]) = (SOLANA, HUB);
const DECIMALS: u8 = 6;
const AMOUNT: u64 = 1_500_000;

fn submit_vaas_case() -> Case {
    let vaa = VaaScenario::direct(
        0xD0,
        SOLANA,
        HUB,
        ETHEREUM,
        SPOKE,
        SOLANA_HUB,
        &transfer_payload(DECIMALS, AMOUNT, ETHEREUM),
    );
    let mut metas = vaa.vaa.shim_metas();
    metas.extend(vaa.metas());
    let mut accounts = vaa.vaa.shim_accounts();
    accounts.extend(vaa.keyed(VaaAccounts::registered(
        SOLANA, HUB, ETHEREUM, SPOKE, SOLANA_HUB,
    )));
    (
        "submit_vaas",
        Instruction::new_with_bytes(
            program_id(),
            &submit_vaas_ix_data(vaa.vaa.guardian_set_bump, &vaa.vaa.body),
            metas,
        ),
        accounts,
    )
}

#[test]
fn rejects_second_instruction() {
    let mollusk = mollusk();
    let obs = ObsScenario::new(
        Observation::direct(SOLANA, HUB, 0xD1, ETHEREUM, DECIMALS, AMOUNT),
        SPOKE,
        SOLANA_HUB,
    );
    let cases: [Case; 2] = [
        submit_vaas_case(),
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
    let (_, ix, mut accounts) = submit_vaas_case();

    let mut metas = vec![AccountMeta::new_readonly(program_id(), false)];
    metas.extend(ix.accounts);
    accounts.push((
        program_id(),
        create_program_account_loader_v3(&program_id()),
    ));
    let forwarded = Instruction::new_with_bytes(forwarder, &ix.data, metas);
    let result = mollusk.process_instruction(&forwarded, &accounts);

    assert_error(
        &result,
        GlobalAccountantError::CpiInvocation as u64,
        "submit_vaas through CPI",
    );
}
