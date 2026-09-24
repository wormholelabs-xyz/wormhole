use global_accountant_definitions::GlobalAccountantError;
use mollusk_svm::Mollusk;
use solana_account::Account;

use crate::common::*;

fn marked_by_vaas(mollusk: &Mollusk, transfer: Transfer) -> Account {
    let vaas = VaaScenario::transfer(transfer);
    let result = vaas.submit(mollusk, vaas.accounts());
    assert_success(&result, "submit_vaas");
    find_account(&result.resulting_accounts, &vaas.noreplay_bucket).clone()
}

fn marked_by_observations(mollusk: &Mollusk, transfer: Transfer) -> Account {
    let obs = ObsScenario::transfer(GUARDIAN_SET_INDEX, 0x42, transfer);
    let accounts = obs.submit_n(mollusk, QUORUM);
    find_account(&accounts, &obs.noreplay_bucket).clone()
}

fn replay_via_vaas(mollusk: &Mollusk, transfer: Transfer, bucket: Account) -> Option<u64> {
    let vaas = VaaScenario::transfer(transfer);
    let mut accounts = vaas.accounts();
    replace_account(&mut accounts, &vaas.noreplay_bucket, bucket);
    error_code(&vaas.submit(mollusk, accounts).program_result)
}

fn replay_via_observations(mollusk: &Mollusk, transfer: Transfer, bucket: Account) -> Option<u64> {
    let obs = ObsScenario::transfer(GUARDIAN_SET_INDEX, 0x42, transfer);
    let mut accounts = obs.initial_accounts();
    replace_account(&mut accounts, &obs.noreplay_bucket, bucket);
    error_code(&obs.submit_once(mollusk, accounts, 0).program_result)
}

#[test]
fn paths_share_one_replay_slot() {
    let mollusk = mollusk();
    type Mark = fn(&Mollusk, Transfer) -> Account;
    type Replay = fn(&Mollusk, Transfer, Account) -> Option<u64>;
    let cases: [(&str, Transfer, Mark, Replay); 2] = [
        (
            "vaas then observations",
            Transfer::new(0xB0, ETHEREUM, SOLANA, 1_000),
            marked_by_vaas,
            replay_via_observations,
        ),
        (
            "observations then vaas",
            Transfer::new(0xB1, ETHEREUM, SOLANA, 2_000),
            marked_by_observations,
            replay_via_vaas,
        ),
    ];
    for (label, transfer, mark, replay) in cases {
        let bucket = mark(&mollusk, transfer);
        assert_bucket_marked(&bucket, transfer.sequence);
        assert_eq!(
            replay(&mollusk, transfer, bucket),
            Some(GlobalAccountantError::AlreadyAccounted as u64),
            "{label}"
        );
    }
}
