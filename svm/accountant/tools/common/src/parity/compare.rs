//! Key-by-key comparison of the expected state (snapshot) and the actual state (Solana).

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::ids::MessageId;
use crate::parity::state::{
    marked_messages, BucketBits, ChainState, FindingKind, KeyedKind, StateKind,
};

/// One finding. `None` on a side means the key is absent there. The serialized
/// sides keep the ledger names: `wormchain` is the expected side, `solana` the actual side.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Finding {
    pub kind: FindingKind,
    pub key: String,
    #[serde(rename = "wormchain")]
    pub expected: Option<String>,
    #[serde(rename = "solana")]
    pub actual: Option<String>,
}

/// Compare `expected` against `actual`, all states.
/// Output order is deterministic: kind order, then key order.
pub fn compare(expected: &ChainState, actual: &ChainState) -> Vec<Finding> {
    let mut out = Vec::new();

    let render = |value: &[u8]| format!("0x{}", hex::encode(value));
    for kind in KeyedKind::ALL {
        let expected_rows = expected.rows(kind);
        let actual_rows = actual.rows(kind);
        let keys: BTreeSet<&Vec<u8>> = expected_rows.keys().chain(actual_rows.keys()).collect();
        for key in keys {
            let (expected_value, actual_value) = (expected_rows.get(key), actual_rows.get(key));
            if expected_value != actual_value {
                out.push(Finding {
                    kind: kind.into(),
                    key: kind.describe_key(key),
                    expected: expected_value.map(|v| render(v)),
                    actual: actual_value.map(|v| render(v)),
                });
            }
        }
    }

    // Per bucket: whole bitmaps compare first, differing bits expand to message ids.
    const EMPTY: BucketBits = [0; global_accountant_definitions::NOREPLAY_BITMAP_BYTES];
    let expected_buckets = expected.replay_marks.buckets();
    let actual_buckets = actual.replay_marks.buckets();
    let buckets: BTreeSet<_> = expected_buckets
        .keys()
        .chain(actual_buckets.keys())
        .collect();
    for bucket in buckets {
        let expected_mask = expected_buckets.get(bucket).unwrap_or(&EMPTY);
        let actual_mask = actual_buckets.get(bucket).unwrap_or(&EMPTY);
        if expected_mask == actual_mask {
            continue;
        }
        let mut only_expected = EMPTY;
        let mut only_actual = EMPTY;
        for i in 0..EMPTY.len() {
            only_expected[i] = expected_mask[i] & !actual_mask[i];
            only_actual[i] = actual_mask[i] & !expected_mask[i];
        }
        let mut changed_bits: Vec<(MessageId, bool)> = marked_messages(bucket, &only_expected)
            .map(|message| (message, true))
            .chain(marked_messages(bucket, &only_actual).map(|message| (message, false)))
            .collect();
        changed_bits.sort();
        for (message, is_expected) in changed_bits {
            out.push(Finding {
                kind: FindingKind::State(StateKind::ReplayMarks),
                key: message.to_string(),
                expected: is_expected.then(|| "marked".to_string()),
                actual: (!is_expected).then(|| "marked".to_string()),
            });
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::{Address, ChainId, EmitterKey, Sequence};

    fn balance_key(chain: u16, token_chain: u16, addr: u8) -> Vec<u8> {
        let mut key = chain.to_be_bytes().to_vec();
        key.extend_from_slice(&token_chain.to_be_bytes());
        key.extend_from_slice(&[addr; 32]);
        key
    }

    fn state(balances: &[(u16, u16, u8, u8)], replay: &[(u16, u64)]) -> ChainState {
        let mut out = ChainState::default();
        for (chain, token_chain, addr, value) in balances {
            out.insert(
                KeyedKind::Balances,
                balance_key(*chain, *token_chain, *addr),
                vec![*value; 32],
            )
            .unwrap();
        }
        for (chain, sequence) in replay {
            let message = MessageId {
                emitter: EmitterKey {
                    chain: ChainId(*chain),
                    emitter: Address([0xEE; 32]),
                },
                sequence: Sequence(*sequence),
            };
            out.replay_marks.insert(message).unwrap();
        }
        out
    }

    /// `(finding kind, expected side present, actual side present)`.
    type FindingShape = (FindingKind, bool, bool);

    #[test]
    fn finding_table() {
        let balances = FindingKind::State(StateKind::BALANCES);
        let replay = FindingKind::State(StateKind::ReplayMarks);
        let cases: [(&str, ChainState, ChainState, usize, &[FindingShape]); 6] = [
            (
                "equal states",
                state(&[(1, 2, 0xAA, 7)], &[(2, 5)]),
                state(&[(1, 2, 0xAA, 7)], &[(2, 5)]),
                0,
                &[],
            ),
            (
                "balance value mismatch",
                state(&[(1, 2, 0xAA, 7)], &[]),
                state(&[(1, 2, 0xAA, 8)], &[]),
                1,
                &[(balances, true, true)],
            ),
            (
                "balance missing on solana",
                state(&[(1, 2, 0xAA, 7)], &[]),
                state(&[], &[]),
                1,
                &[(balances, true, false)],
            ),
            (
                "balance only on solana",
                state(&[], &[]),
                state(&[(1, 2, 0xAA, 7)], &[]),
                1,
                &[(balances, false, true)],
            ),
            (
                "replay mark missing on solana",
                state(&[], &[(2, 5)]),
                state(&[], &[]),
                1,
                &[(replay, true, false)],
            ),
            (
                "replay mark only on solana",
                state(&[], &[]),
                state(&[], &[(2, 5)]),
                1,
                &[(replay, false, true)],
            ),
        ];
        for (name, expected, actual, count, shapes) in cases {
            let found = compare(&expected, &actual);
            assert_eq!(found.len(), count, "{name}: finding count");
            for (finding, (kind, has_expected, has_actual)) in found.iter().zip(shapes) {
                assert_eq!(finding.kind, *kind, "{name}: kind");
                assert_eq!(
                    finding.expected.is_some(),
                    *has_expected,
                    "{name}: expected side"
                );
                assert_eq!(finding.actual.is_some(), *has_actual, "{name}: actual side");
            }
        }
    }

    #[test]
    fn finding_serializes_with_ledger_side_names() {
        let finding = Finding {
            kind: FindingKind::MalformedAccounts,
            key: "k".into(),
            expected: None,
            actual: Some("x".into()),
        };
        let json = serde_json::to_value(&finding).unwrap();
        assert_eq!(
            json,
            serde_json::json!({"kind": "malformed_accounts", "key": "k", "wormchain": null, "solana": "x"})
        );
    }
}
