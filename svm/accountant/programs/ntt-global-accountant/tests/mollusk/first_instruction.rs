//! `submit_observations` and `submit_vaas` run only as the first top-level instruction. An
//! earlier instruction can fill the transaction log and drop the commit log.

use global_accountant_definitions::GlobalAccountantError;
use solana_account::Account;
use solana_instruction::Instruction;
use solana_pubkey::Pubkey;

use crate::common::*;

/// Label, accountant instruction, initial accounts.
type Case = (&'static str, Instruction, Vec<(Pubkey, Account)>);

const SOLANA_HUB: (u16, [u8; 32]) = (SOLANA, HUB);
const DECIMALS: u8 = 6;
const AMOUNT: u64 = 1_500_000;

#[test]
fn rejects_second_instruction() {
    let mollusk = mollusk();
    let vaa = VaaScenario::direct(
        0xD0,
        SOLANA,
        HUB,
        ETHEREUM,
        SPOKE,
        SOLANA_HUB,
        &transfer_payload(DECIMALS, AMOUNT, ETHEREUM),
    );
    let mut vaa_metas = vaa.vaa.shim_metas();
    vaa_metas.extend(vaa.metas());
    let mut vaa_accounts = vaa.vaa.shim_accounts();
    vaa_accounts.extend(vaa.keyed(VaaAccounts::registered(
        SOLANA, HUB, ETHEREUM, SPOKE, SOLANA_HUB,
    )));
    let obs = ObsScenario::new(
        Observation::direct(SOLANA, HUB, 0xD1, ETHEREUM, DECIMALS, AMOUNT),
        SPOKE,
        SOLANA_HUB,
    );

    let cases: [Case; 2] = [
        (
            "submit_vaas",
            Instruction::new_with_bytes(
                program_id(),
                &submit_vaas_ix_data(vaa.vaa.guardian_set_bump, &vaa.vaa.body),
                vaa_metas,
            ),
            vaa_accounts,
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
