//! CU regression tracking for the hot, per-VAA paths. Run via `just bench`;
//! results land in `benches/compute_units.md`, tracked so a CU regression
//! shows up as a diff in review.
//!
//! `submit_observations` benches two cases: the common per-guardian path
//! (writes the pending PDA, no quorum yet) and the quorum-closing path (the
//! same instruction on the observation that crosses quorum, which additionally
//! applies the transfer, marks NoReplay, and emits the commit log — the
//! expensive branch already CU-ceiling-tested in `tests/mollusk/submit_observations.rs`).

#[path = "../tests/common/mod.rs"]
mod common;

use mollusk_svm_bencher::MolluskComputeUnitBencher;
use solana_instruction::Instruction;

use common::{
    mollusk, program_id, submit_vaas_ix_data, ObsScenario, Transfer, VaaScenario, ETHEREUM,
    GUARDIAN_SET_INDEX, QUORUM, SOLANA,
};

fn main() {
    let vaa = VaaScenario::transfer(Transfer::new(0xA0, ETHEREUM, SOLANA, 500_000));
    let vaa_accounts = vaa.accounts();
    let vaa_ix = Instruction::new_with_bytes(
        program_id(),
        &submit_vaas_ix_data(vaa.guardian_set_bump, &vaa.body),
        vaa.account_metas(),
    );

    let obs_first = ObsScenario::transfer(GUARDIAN_SET_INDEX, 0x42, Transfer::new(1, ETHEREUM, SOLANA, 500_000));
    let obs_first_accounts = obs_first.initial_accounts();
    let obs_first_ix =
        Instruction::new_with_bytes(program_id(), &obs_first.ix_data(0), obs_first.account_metas());

    let obs_quorum = ObsScenario::transfer(GUARDIAN_SET_INDEX, 0x42, Transfer::new(2, ETHEREUM, SOLANA, 500_000));
    let pre_quorum_accounts =
        obs_quorum.submit_range(&mollusk(), obs_quorum.initial_accounts(), 0..(QUORUM - 1));
    let obs_quorum_ix = Instruction::new_with_bytes(
        program_id(),
        &obs_quorum.ix_data(QUORUM - 1),
        obs_quorum.account_metas(),
    );

    let mut bencher = MolluskComputeUnitBencher::new(mollusk())
        .bench(("submit_vaas: transfer", &vaa_ix, &vaa_accounts))
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
