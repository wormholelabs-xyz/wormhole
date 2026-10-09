//! Key-by-key comparison of the expected (wormchain) and on-chain (Solana) states.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::state::{marked_messages, BucketBits, ChainState, ComparedState, MessageId};

/// One finding. `None` on a side means the key is absent there.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Finding {
    pub kind: ComparedState,
    pub key: String,
    pub wormchain: Option<String>,
    pub solana: Option<String>,
}

/// Compare `wormchain` (expected) against `solana` (actual), all states.
/// Output order is deterministic: compared-state order, then key order.
pub fn compare(wormchain: &ChainState, solana: &ChainState) -> Vec<Finding> {
    let mut out = Vec::new();

    for kind in ComparedState::ACCOUNT_BACKED {
        let render = |value: &[u8]| format!("0x{}", hex::encode(value));
        let expected = wormchain.rows(kind);
        let actual = solana.rows(kind);
        let keys: BTreeSet<&Vec<u8>> = expected.keys().chain(actual.keys()).collect();
        for key in keys {
            let (left, right) = (expected.get(key), actual.get(key));
            if left != right {
                out.push(Finding {
                    kind,
                    key: kind.describe_key(key),
                    wormchain: left.map(|v| render(v)),
                    solana: right.map(|v| render(v)),
                });
            }
        }
    }

    // Per bucket: whole bitmaps compare first, differing bits expand to message ids.
    const EMPTY: BucketBits = [0; global_accountant_definitions::NOREPLAY_BITMAP_BYTES];
    let expected = wormchain.replay.buckets();
    let actual = solana.replay.buckets();
    let buckets: BTreeSet<_> = expected.keys().chain(actual.keys()).collect();
    for bucket in buckets {
        let left = expected.get(bucket).unwrap_or(&EMPTY);
        let right = actual.get(bucket).unwrap_or(&EMPTY);
        if left == right {
            continue;
        }
        let mut only_left = EMPTY;
        let mut only_right = EMPTY;
        for i in 0..EMPTY.len() {
            only_left[i] = left[i] & !right[i];
            only_right[i] = right[i] & !left[i];
        }
        let mut changed_bits: Vec<(MessageId, bool)> = marked_messages(bucket, &only_left)
            .map(|k| (k, true))
            .chain(marked_messages(bucket, &only_right).map(|k| (k, false)))
            .collect();
        changed_bits.sort();
        for (key, on_wormchain) in changed_bits {
            out.push(Finding {
                kind: ComparedState::ReplayMarks,
                key: describe_message(&key),
                wormchain: on_wormchain.then(|| "marked".to_string()),
                solana: (!on_wormchain).then(|| "marked".to_string()),
            });
        }
    }

    out
}

pub fn describe_message((chain, emitter, sequence): &MessageId) -> String {
    format!(
        "chain={chain} emitter=0x{} sequence={sequence}",
        hex::encode(emitter)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

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
                ComparedState::Balances,
                balance_key(*chain, *token_chain, *addr),
                vec![*value; 32],
            )
            .unwrap();
        }
        for (chain, sequence) in replay {
            out.replay.insert((*chain, [0xEE; 32], *sequence)).unwrap();
        }
        out
    }

    /// `(compared state, wormchain side present, solana side present)`.
    type DiffShape = (ComparedState, bool, bool);

    #[test]
    fn diff_table() {
        let cases: [(&str, ChainState, ChainState, usize, &[DiffShape]); 6] = [
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
                &[(ComparedState::Balances, true, true)],
            ),
            (
                "balance missing on solana",
                state(&[(1, 2, 0xAA, 7)], &[]),
                state(&[], &[]),
                1,
                &[(ComparedState::Balances, true, false)],
            ),
            (
                "balance only on solana",
                state(&[], &[]),
                state(&[(1, 2, 0xAA, 7)], &[]),
                1,
                &[(ComparedState::Balances, false, true)],
            ),
            (
                "replay mark missing on solana",
                state(&[], &[(2, 5)]),
                state(&[], &[]),
                1,
                &[(ComparedState::ReplayMarks, true, false)],
            ),
            (
                "replay mark only on solana",
                state(&[], &[]),
                state(&[], &[(2, 5)]),
                1,
                &[(ComparedState::ReplayMarks, false, true)],
            ),
        ];
        for (name, wormchain, solana, count, shapes) in cases {
            let found = compare(&wormchain, &solana);
            assert_eq!(found.len(), count, "{name}: diff count");
            for (entry, (kind, has_wormchain, has_solana)) in found.iter().zip(shapes) {
                assert_eq!(entry.kind, *kind, "{name}: kind");
                assert_eq!(
                    entry.wormchain.is_some(),
                    *has_wormchain,
                    "{name}: wormchain side"
                );
                assert_eq!(entry.solana.is_some(), *has_solana, "{name}: solana side");
            }
        }
    }
}
