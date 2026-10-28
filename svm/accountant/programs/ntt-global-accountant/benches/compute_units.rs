//! CU regression tracking for the hot, per-message paths. Run via `just bench`; results land
//! in `benches/compute_units.md`, tracked so a CU regression shows up as a diff in review.
//!
//! `submit_vaas` benches a direct publish and a relayed one, which adds the
//! `DeliveryInstruction` unwrap. `submit_observations` benches the common per-guardian path
//! and the quorum-closing path, which additionally normalizes the amount, checks both peers,
//! moves balances, marks NoReplay and emits the commit log.

#[path = "../tests/common/mod.rs"]
mod common;

use global_accountant_definitions::Uint256;
use mollusk_svm_bencher::MolluskComputeUnitBencher;
use solana_account::Account;
use solana_instruction::Instruction;
use solana_pubkey::Pubkey;

use common::*;

const SOLANA_HUB: (u16, [u8; 32]) = (SOLANA, HUB);
const DECIMALS: u8 = 6;
const AMOUNT: u64 = 1_500_000;
const BOOKED: u128 = 150_000_000;

fn vaa_case(
    scenario: &VaaScenario,
    accounts: VaaAccounts,
) -> (Instruction, Vec<(Pubkey, Account)>) {
    let ix = Instruction::new_with_bytes(
        program_id(),
        &submit_vaas_ix_data(scenario.vaa.guardian_set_bump, &scenario.vaa.body),
        [scenario.vaa.shim_metas(), scenario.metas()].concat(),
    );
    let accounts = [scenario.vaa.shim_accounts(), scenario.keyed(accounts)].concat();
    (ix, accounts)
}

fn main() {
    let direct = VaaScenario::direct(
        1,
        SOLANA,
        HUB,
        ETHEREUM,
        SPOKE,
        SOLANA_HUB,
        &transfer_payload(DECIMALS, AMOUNT, ETHEREUM),
    );
    let (direct_ix, direct_accounts) = vaa_case(
        &direct,
        VaaAccounts::registered(SOLANA, HUB, ETHEREUM, SPOKE, SOLANA_HUB),
    );

    let relayed = VaaScenario::relayed(
        2,
        ETHEREUM,
        RELAYER,
        SPOKE,
        SOLANA,
        HUB,
        SOLANA_HUB,
        &transfer_payload(DECIMALS, AMOUNT, SOLANA),
    );
    let (relayed_ix, relayed_accounts) = vaa_case(
        &relayed,
        VaaAccounts {
            relayer_registration: chain_registration_account_for(&program_id(), ETHEREUM, RELAYER),
            source_balance: balance_account(ETHEREUM, SOLANA, HUB, Uint256::from_u128(BOOKED)),
            dest_balance: balance_account(SOLANA, SOLANA, HUB, Uint256::from_u128(BOOKED)),
            ..VaaAccounts::registered(ETHEREUM, SPOKE, SOLANA, HUB, SOLANA_HUB)
        },
    );

    let obs_first = ObsScenario::new(
        Observation::direct(SOLANA, HUB, 3, ETHEREUM, DECIMALS, AMOUNT),
        SPOKE,
        SOLANA_HUB,
    );
    let obs_first_accounts = obs_first.initial_accounts();
    let obs_first_ix = Instruction::new_with_bytes(
        program_id(),
        &obs_first.ix_data(0),
        obs_first.account_metas(),
    );

    let obs_quorum = ObsScenario::new(
        Observation::direct(SOLANA, HUB, 4, ETHEREUM, DECIMALS, AMOUNT),
        SPOKE,
        SOLANA_HUB,
    );
    let pre_quorum_accounts =
        obs_quorum.submit_range(&mollusk(), obs_quorum.initial_accounts(), 0..(QUORUM - 1));
    let obs_quorum_ix = Instruction::new_with_bytes(
        program_id(),
        &obs_quorum.ix_data(QUORUM - 1),
        obs_quorum.account_metas(),
    );

    let mut bencher = MolluskComputeUnitBencher::new(mollusk())
        .bench(("submit_vaas: direct transfer", &direct_ix, &direct_accounts))
        .bench((
            "submit_vaas: relayed transfer",
            &relayed_ix,
            &relayed_accounts,
        ))
        .bench((
            "submit_observations: single guardian",
            &obs_first_ix,
            &obs_first_accounts,
        ))
        .bench((
            "submit_observations: quorum-closing",
            &obs_quorum_ix,
            &pre_quorum_accounts,
        ))
        .must_pass(true);
    bencher.execute();
}
