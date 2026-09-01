//! `BackfillBalance` / `BackfillNoReplay` instruction-data encoders, built on the
//! `bytemuck` structs in `global_accountant_definitions`. One encoder per format;
//! every test call site in this suite uses these instead of hand-rolled bytes.

use global_accountant_definitions::{
    BackfillBalanceEntry, BackfillInstruction, BackfillNoReplayEntry, BackfillNoReplayGroupHeader,
    Uint256,
};

/// `chain`/`token_chain` as `u16`, `balance` as raw big-endian bytes.
pub fn balance_entry(
    chain: u16,
    token_chain: u16,
    token_address: [u8; 32],
    balance: [u8; 32],
) -> BackfillBalanceEntry {
    BackfillBalanceEntry::new(
        chain,
        token_chain,
        token_address,
        Uint256::from_be_bytes(balance),
    )
}

/// `BackfillBalance` instruction data: `disc ‖ count ‖ count × BackfillBalanceEntry`.
pub fn encode_balance_batch(entries: &[BackfillBalanceEntry]) -> Vec<u8> {
    let mut data = vec![
        BackfillInstruction::BackfillBalance as u8,
        entries.len() as u8,
    ];
    for entry in entries {
        data.extend_from_slice(bytemuck::bytes_of(entry));
    }
    data
}

/// One `(chain, emitter)` group's sequences, unencoded.
pub type RawGroup<'a> = (u16, [u8; 32], &'a [(u64, [u8; 32])]);

/// Encodes each group exactly as given: bytes come from the structs, but group order
/// and duplication are the caller's responsibility. For malformed-wire tests that need
/// unsorted or duplicate groups a well-formed builder cannot produce.
pub fn encode_noreplay_batch_raw(groups: &[RawGroup]) -> Vec<u8> {
    let mut data = vec![
        BackfillInstruction::BackfillNoReplay as u8,
        groups.len() as u8,
    ];
    for (chain, emitter, entries) in groups {
        data.extend_from_slice(bytemuck::bytes_of(&BackfillNoReplayGroupHeader::new(
            *chain,
            *emitter,
            entries.len() as u8,
        )));
        for &(sequence, digest) in entries.iter() {
            data.extend_from_slice(bytemuck::bytes_of(&BackfillNoReplayEntry::new(
                sequence, digest,
            )));
        }
    }
    data
}

#[derive(Clone, Copy)]
pub struct NoReplayEntry {
    pub chain: u16,
    pub emitter: [u8; 32],
    pub sequence: u64,
    pub digest: [u8; 32],
}

/// One owned `(chain, emitter)` group, accumulated before encoding.
type OwnedGroup = (u16, [u8; 32], Vec<(u64, [u8; 32])>);

/// Groups adjacent same-key entries by `(chain, emitter)`, then encodes. Callers must
/// pre-sort so each group's run is contiguous and its sequences ascending.
pub fn encode_noreplay_batch(entries: &[NoReplayEntry]) -> Vec<u8> {
    let mut groups: Vec<OwnedGroup> = Vec::new();
    for e in entries {
        match groups.last_mut() {
            Some((chain, emitter, seqs)) if *chain == e.chain && *emitter == e.emitter => {
                seqs.push((e.sequence, e.digest));
            }
            _ => groups.push((e.chain, e.emitter, vec![(e.sequence, e.digest)])),
        }
    }
    let raw: Vec<RawGroup> = groups
        .iter()
        .map(|(chain, emitter, seqs)| (*chain, *emitter, seqs.as_slice()))
        .collect();
    encode_noreplay_batch_raw(&raw)
}

#[cfg(test)]
mod tests {
    use super::*;
    use global_accountant_definitions::{BalanceBatch, NoReplayBatch};

    #[test]
    fn balance_batch_round_trips_through_the_parser() {
        let entries = [
            balance_entry(1, 1, [0x11; 32], [0xAA; 32]),
            balance_entry(1, 2, [0x22; 32], [0xBB; 32]),
        ];
        let data = encode_balance_batch(&entries);
        let batch = BalanceBatch::parse(&data[1..]).unwrap();
        assert_eq!(batch.entries(), entries.as_slice());
    }

    #[test]
    fn noreplay_batch_round_trips_through_the_parser() {
        let emitter_a = [0x11; 32];
        let emitter_b = [0x22; 32];
        let entries = [
            NoReplayEntry {
                chain: 1,
                emitter: emitter_a,
                sequence: 1,
                digest: [0xAA; 32],
            },
            NoReplayEntry {
                chain: 1,
                emitter: emitter_a,
                sequence: 2,
                digest: [0xBB; 32],
            },
            NoReplayEntry {
                chain: 1,
                emitter: emitter_b,
                sequence: 5,
                digest: [0xCC; 32],
            },
        ];
        let data = encode_noreplay_batch(&entries);
        let batch = NoReplayBatch::parse(&data[1..]).unwrap();
        let groups: Vec<_> = batch.groups().collect();
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].header.chain(), 1);
        assert_eq!(groups[0].header.emitter, emitter_a);
        assert_eq!(groups[0].entries.len(), 2);
        assert_eq!(groups[0].entries[0].sequence(), 1);
        assert_eq!(groups[0].entries[0].digest, [0xAA; 32]);
        assert_eq!(groups[0].entries[1].sequence(), 2);
        assert_eq!(groups[1].header.emitter, emitter_b);
        assert_eq!(groups[1].entries.len(), 1);
        assert_eq!(groups[1].entries[0].sequence(), 5);
    }

    #[test]
    fn noreplay_batch_raw_matches_grouped_for_well_formed_input() {
        let grouped = encode_noreplay_batch(&[
            NoReplayEntry {
                chain: 2,
                emitter: [0x33; 32],
                sequence: 10,
                digest: [0x01; 32],
            },
            NoReplayEntry {
                chain: 2,
                emitter: [0x33; 32],
                sequence: 20,
                digest: [0x02; 32],
            },
        ]);
        let raw =
            encode_noreplay_batch_raw(&[(2, [0x33; 32], &[(10, [0x01; 32]), (20, [0x02; 32])])]);
        assert_eq!(grouped, raw);
    }
}
