//! Instruction-data framing for the shared handlers: `discriminator ‖ prefix ‖ body`.
//! Each program passes its own discriminator byte.

use global_accountant_definitions::{
    BackfillBalanceEntry, BackfillChainRegistrationEntry, BackfillModifyBalanceEntry,
    BackfillNoReplayEntry, BackfillNoReplayGroupHeader, BackfillTransceiverHubEntry,
    ClosePendingIxData, DeliveryHead, DeliveryMiddle, DeliveryTail, ModifyBalanceIxData,
    RegisterChainIxData, SubmitVaasIxData, Uint256, UpgradeContractIxData,
    DELIVERY_INSTRUCTION_PAYLOAD_ID,
};

/// `prefix` is a `Pod` `*IxData` struct as raw bytes; its `body_len` field must equal
/// `body.len()`. `split_body` reverses this layout and rejects any other length.
pub fn framed(discriminator: u8, prefix: &[u8], body: &[u8]) -> Vec<u8> {
    let mut data = Vec::with_capacity(1 + prefix.len() + body.len());
    data.push(discriminator);
    data.extend_from_slice(prefix);
    data.extend_from_slice(body);
    data
}

pub fn submit_vaas(discriminator: u8, guardian_set_bump: u8, body: &[u8]) -> Vec<u8> {
    submit_vaas_with_len(discriminator, guardian_set_bump, body.len() as u16, body)
}

pub fn submit_vaas_with_len(
    discriminator: u8,
    guardian_set_bump: u8,
    body_len: u16,
    body: &[u8],
) -> Vec<u8> {
    let prefix = SubmitVaasIxData {
        guardian_set_bump,
        body_len: body_len.to_le_bytes(),
    };
    framed(discriminator, bytemuck::bytes_of(&prefix), body)
}

pub fn register_chain(discriminator: u8, guardian_set_bump: u8, body: &[u8]) -> Vec<u8> {
    let prefix = RegisterChainIxData {
        guardian_set_bump,
        body_len: (body.len() as u16).to_le_bytes(),
    };
    framed(discriminator, bytemuck::bytes_of(&prefix), body)
}

pub fn modify_balance(discriminator: u8, guardian_set_bump: u8, body: &[u8]) -> Vec<u8> {
    let prefix = ModifyBalanceIxData {
        guardian_set_bump,
        body_len: (body.len() as u16).to_le_bytes(),
    };
    framed(discriminator, bytemuck::bytes_of(&prefix), body)
}

pub fn upgrade_contract(discriminator: u8, guardian_set_bump: u8, body: &[u8]) -> Vec<u8> {
    let prefix = UpgradeContractIxData {
        guardian_set_bump,
        body_len: (body.len() as u16).to_le_bytes(),
    };
    framed(discriminator, bytemuck::bytes_of(&prefix), body)
}

pub fn close_pending(discriminator: u8, emitter: [u8; 32], sequence: u64) -> Vec<u8> {
    let data = ClosePendingIxData {
        emitter,
        sequence: sequence.to_be_bytes(),
    };
    framed(discriminator, bytemuck::bytes_of(&data), &[])
}

/// Standard Relayer `DeliveryInstruction` from `sender` wrapping `payload`, with no
/// execution info and no message keys.
pub fn delivery_instruction(sender: [u8; 32], payload: &[u8]) -> Vec<u8> {
    let head = DeliveryHead {
        payload_id: DELIVERY_INSTRUCTION_PAYLOAD_ID,
        target_chain: 1u16.to_be_bytes(),
        target_address: [0x01; 32],
        payload_len: (payload.len() as u32).to_be_bytes(),
    };
    let middle = DeliveryMiddle {
        requested_reciever_value: [0; 32],
        extra_reciever_value: [0; 32],
        exec_info_len: [0; 4],
    };
    let tail = DeliveryTail {
        refund_chain: 1u16.to_be_bytes(),
        refund_address: [0x04; 32],
        refund_delivery_provider: [0x05; 32],
        source_delivery_provider: [0x06; 32],
        sender_address: sender,
        num_messages: 0,
    };
    [
        bytemuck::bytes_of(&head),
        payload,
        bytemuck::bytes_of(&middle),
        bytemuck::bytes_of(&tail),
    ]
    .concat()
}

// ---------------------------------------------------------------------------
// Backfill batches: `discriminator ‖ count ‖ count × entry`.
// ---------------------------------------------------------------------------

/// `balance` as raw big-endian bytes, the wormchain snapshot's own encoding.
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

/// `amount` as raw big-endian bytes.
#[allow(clippy::too_many_arguments)]
pub fn modify_balance_entry(
    kind: u8,
    chain_id: u16,
    token_chain: u16,
    sequence: u64,
    token_address: [u8; 32],
    amount: [u8; 32],
    reason: [u8; 32],
) -> BackfillModifyBalanceEntry {
    BackfillModifyBalanceEntry::new(
        kind,
        chain_id,
        token_chain,
        sequence,
        token_address,
        Uint256::from_be_bytes(amount),
        reason,
    )
}

pub fn chain_registration_entry(
    chain: u16,
    sequence: u64,
    emitter: [u8; 32],
) -> BackfillChainRegistrationEntry {
    BackfillChainRegistrationEntry::new(chain, sequence, emitter)
}

pub fn transceiver_hub_entry(
    chain: u16,
    address: [u8; 32],
    hub_chain: u16,
    hub_address: [u8; 32],
) -> BackfillTransceiverHubEntry {
    BackfillTransceiverHubEntry::new(chain, address, hub_chain, hub_address)
}

fn encode_batch<T: bytemuck::Pod>(discriminator: u8, entries: &[T]) -> Vec<u8> {
    let mut data = vec![discriminator, entries.len() as u8];
    for entry in entries {
        data.extend_from_slice(bytemuck::bytes_of(entry));
    }
    data
}

pub fn encode_balance_batch(discriminator: u8, entries: &[BackfillBalanceEntry]) -> Vec<u8> {
    encode_batch(discriminator, entries)
}

pub fn encode_modify_balance_batch(
    discriminator: u8,
    entries: &[BackfillModifyBalanceEntry],
) -> Vec<u8> {
    encode_batch(discriminator, entries)
}

pub fn encode_chain_registration_batch(
    discriminator: u8,
    entries: &[BackfillChainRegistrationEntry],
) -> Vec<u8> {
    encode_batch(discriminator, entries)
}

pub fn encode_transceiver_hub_batch(
    discriminator: u8,
    entries: &[BackfillTransceiverHubEntry],
) -> Vec<u8> {
    encode_batch(discriminator, entries)
}

/// One `(chain, emitter)` group's sequences, unencoded.
pub type RawGroup<'a> = (u16, [u8; 32], &'a [(u64, [u8; 32])]);

/// Encodes each group exactly as given: group order and duplication are the caller's
/// responsibility, for malformed-wire cases a well-formed builder cannot produce.
pub fn encode_noreplay_batch_raw(discriminator: u8, groups: &[RawGroup]) -> Vec<u8> {
    let mut data = vec![discriminator, groups.len() as u8];
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

/// Groups adjacent same-key entries by `(chain, emitter)`, then encodes. Callers pre-sort so
/// each group's run is contiguous and its sequences ascending.
pub fn encode_noreplay_batch(discriminator: u8, entries: &[NoReplayEntry]) -> Vec<u8> {
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
    encode_noreplay_batch_raw(discriminator, &raw)
}

#[cfg(test)]
mod tests {
    use super::*;
    use global_accountant_definitions::{
        BalanceBatch, ChainRegistrationBatch, ModifyBalanceBatch, NoReplayBatch,
        TransceiverHubBatch,
    };

    /// Any byte: these encoders frame a batch, the discriminator names the program's arm.
    const DISC: u8 = 0x5A;

    #[test]
    fn balance_batch_round_trips_through_the_parser() {
        let entries = [
            balance_entry(1, 1, [0x11; 32], [0xAA; 32]),
            balance_entry(1, 2, [0x22; 32], [0xBB; 32]),
        ];
        let data = encode_balance_batch(DISC, &entries);
        assert_eq!(data[0], DISC);
        let batch = BalanceBatch::parse(&data[1..]).unwrap();
        assert_eq!(batch.entries(), entries.as_slice());
    }

    #[test]
    fn modify_balance_batch_round_trips_through_the_parser() {
        let entries = [
            modify_balance_entry(1, 1, 1, 100, [0x11; 32], [0xAA; 32], [0x01; 32]),
            modify_balance_entry(2, 1, 2, 101, [0x22; 32], [0xBB; 32], [0x02; 32]),
        ];
        let data = encode_modify_balance_batch(DISC, &entries);
        assert_eq!(data[0], DISC);
        let batch = ModifyBalanceBatch::parse(&data[1..]).unwrap();
        assert_eq!(batch.entries(), entries.as_slice());
    }

    #[test]
    fn chain_registration_batch_round_trips_through_the_parser() {
        let entries = [
            chain_registration_entry(2, 100, [0x11; 32]),
            chain_registration_entry(4, 7, [0x22; 32]),
        ];
        let data = encode_chain_registration_batch(DISC, &entries);
        assert_eq!(data[0], DISC);
        let batch = ChainRegistrationBatch::parse(&data[1..]).unwrap();
        assert_eq!(batch.entries(), entries.as_slice());
    }

    #[test]
    fn transceiver_hub_batch_round_trips_through_the_parser() {
        let entries = [
            transceiver_hub_entry(1, [0x7B; 32], 1, [0x7B; 32]),
            transceiver_hub_entry(2, [0x11; 32], 1, [0x7B; 32]),
        ];
        let data = encode_transceiver_hub_batch(DISC, &entries);
        assert_eq!(data[0], DISC);
        let batch = TransceiverHubBatch::parse(&data[1..]).unwrap();
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
        let data = encode_noreplay_batch(DISC, &entries);
        assert_eq!(data[0], DISC);
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
        let grouped = encode_noreplay_batch(
            DISC,
            &[
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
            ],
        );
        let raw = encode_noreplay_batch_raw(
            DISC,
            &[(2, [0x33; 32], &[(10, [0x01; 32]), (20, [0x02; 32])])],
        );
        assert_eq!(grouped, raw);
    }
}
