//! `submit_vaas` and `submit_observations` share one NoReplay slot per
//! `(chain, emitter, sequence)`: whichever path commits first, the other is rejected.

use global_accountant_definitions::GlobalAccountantError;

use crate::common::*;

const SOLANA_HUB: (u16, [u8; 32]) = (SOLANA, HUB);
const SEQUENCE: u64 = 0x42;

fn vaa_path() -> (VaaScenario, VaaAccounts) {
    let payload = transfer_payload(6, 1_500_000, ETHEREUM);
    (
        VaaScenario::direct(SEQUENCE, SOLANA, HUB, ETHEREUM, SPOKE, SOLANA_HUB, &payload),
        VaaAccounts::registered(SOLANA, HUB, ETHEREUM, SPOKE, SOLANA_HUB),
    )
}

fn obs_path() -> ObsScenario {
    ObsScenario::new(
        Observation::direct(SOLANA, HUB, SEQUENCE, ETHEREUM, 6, 1_500_000),
        SPOKE,
        SOLANA_HUB,
    )
}

#[test]
fn paths_share_one_replay_slot() {
    let mollusk = mollusk();

    // VAA first, then an observation for the same message.
    let (vaa, accounts) = vaa_path();
    let committed = vaa.submit(&mollusk, accounts);
    assert_success(&committed, "vaa commit");
    let bucket = find_account(&committed.resulting_accounts, &vaa.noreplay_bucket).clone();
    let obs = obs_path();
    assert_eq!(
        obs.noreplay_bucket, vaa.noreplay_bucket,
        "one slot for both paths"
    );
    let mut accounts = obs.initial_accounts();
    replace_account(&mut accounts, &obs.noreplay_bucket, bucket);
    let replay = obs.submit_once(&mollusk, accounts, 0);
    assert_error(
        &replay,
        GlobalAccountantError::AlreadyAccounted as u64,
        "observation after vaa",
    );

    // Observation quorum first, then the VAA.
    let obs = obs_path();
    let after = obs.submit_range(&mollusk, obs.initial_accounts(), 0..13);
    let bucket = find_account(&after, &obs.noreplay_bucket).clone();
    let (vaa, accounts) = vaa_path();
    let replay = vaa.submit(&mollusk, VaaAccounts { bucket, ..accounts });
    assert_error(
        &replay,
        GlobalAccountantError::AlreadyAccounted as u64,
        "vaa after observation quorum",
    );
}
