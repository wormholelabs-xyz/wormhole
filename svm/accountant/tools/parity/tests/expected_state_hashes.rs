//! Canonical state from the real snapshot fixtures shared with
//! `tools/backfill`. Counts and content hashes are pinned: a change here
//! means either the fixture or the state/hash encoding moved — both must be
//! deliberate.

use std::path::PathBuf;

use ga_backfill::accountant::Accountant;
use ga_parity::expected_state::expected_state_from_snapshot;
use ga_parity::state::{hex32, ComparedState};

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../backfill/tests/fixtures")
        .join(name)
}

#[test]
fn fixture_states_are_pinned() {
    let cases: [(&str, Accountant, usize, usize, &str, &str); 2] = [
        (
            "sample_snapshot.jsonl",
            Accountant::Wtt,
            1,
            2,
            "0x973098895e4978129e1ab27da341ee219a879b38e044930a62cb61413b3ea5bd",
            "0x5237b152228f1e080902a96b73af25930a571d51ec2ce7486a16f1772a02850c",
        ),
        (
            "ntt_snapshot_mainnet_subset.jsonl",
            Accountant::Ntt,
            9,
            30,
            "0xaccf36530b62e0ec89a8a17ab25279bc33c2df58dfc21a00e20170d1d023b2f3",
            "0x1862b954d320bb5ca74775a75aa8c11bf294a8eed80e33d241828147a6f3417b",
        ),
    ];
    for (name, accountant, balances, replay, balances_hash, replay_hash) in cases {
        let (state, _) = expected_state_from_snapshot(&fixture(name), accountant).unwrap();
        assert_eq!(
            state.state_len(ComparedState::Balances),
            balances,
            "{name}: balance count"
        );
        assert_eq!(
            state.state_len(ComparedState::ReplayMarks),
            replay,
            "{name}: replay count"
        );
        assert_eq!(
            hex32(&state.state_hash(ComparedState::Balances)),
            balances_hash,
            "{name}: balances hash",
        );
        assert_eq!(
            hex32(&state.state_hash(ComparedState::ReplayMarks)),
            replay_hash,
            "{name}: replay hash"
        );
    }
}
