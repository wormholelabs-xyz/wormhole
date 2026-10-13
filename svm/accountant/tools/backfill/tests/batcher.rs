//! Integration tests for the snapshot → batch planner.

mod common;

use global_accountant_definitions::Uint256;
use svm_accountant_backfill::batcher::{
    Batcher, TxBatch, MAX_BALANCE_ENTRIES_PER_BATCH, MAX_CHAIN_REGISTRATION_ENTRIES_PER_BATCH,
    MAX_MODIFY_BALANCE_ENTRIES_PER_BATCH, MAX_NOREPLAY_ENTRIES_PER_BATCH,
    MAX_TRANSCEIVER_HUB_ENTRIES_PER_BATCH, MAX_TRANSCEIVER_PEER_ENTRIES_PER_BATCH,
};
use svm_accountant_backfill::target::{AccountantProgram, BackfillTarget};
use svm_accountant_tools_common::accountant::Accountant;
use svm_accountant_tools_common::ids::{Address, BalanceKey, ChainId, Digest, Sequence};
use svm_accountant_tools_common::snapshot::{
    BalanceRow, ChainRegistrationRow, ModificationKind, ModifyBalanceRow, SnapshotRow,
    TransceiverHubRow, TransceiverPeerRow, TransferRow,
};

fn make_relayer(chain: u16) -> ChainRegistrationRow {
    let mut registered_emitter = [0u8; 32];
    registered_emitter[31] = (chain & 0xff) as u8;
    ChainRegistrationRow {
        chain: ChainId(chain),
        registered_emitter: Address(registered_emitter),
        sequence: Sequence(u64::from(chain)),
    }
}

fn target(accountant: Accountant) -> BackfillTarget {
    common::target(accountant, accountant.program_id(), accountant.authority())
}

fn make_hub(chain: u16, addr_seed: u8) -> TransceiverHubRow {
    let mut address = [0u8; 32];
    address[31] = addr_seed;
    TransceiverHubRow {
        chain: ChainId(chain),
        address: Address(address),
        hub_chain: ChainId(1),
        hub_address: Address([0u8; 32]),
    }
}

fn make_peer(chain: u16, addr_seed: u8, dest_chain: u16) -> TransceiverPeerRow {
    let mut address = [0u8; 32];
    address[31] = addr_seed;
    TransceiverPeerRow {
        chain: ChainId(chain),
        address: Address(address),
        dest_chain: ChainId(dest_chain),
        peer_address: Address([0u8; 32]),
    }
}

fn make_transfer(chain: u16, emitter_seed: u8, sequence: u64) -> TransferRow {
    let mut emitter = [0u8; 32];
    emitter[31] = emitter_seed;
    let mut digest = [0u8; 32];
    digest[31] = (sequence & 0xff) as u8;
    TransferRow {
        chain: ChainId(chain),
        emitter: Address(emitter),
        sequence: Sequence(sequence),
        digest: Digest(digest),
        amount: Uint256::ZERO,
        token_chain: ChainId(1),
        token_address: Address([0u8; 32]),
        recipient_chain: ChainId(2),
    }
}

fn make_balance(chain: u16, token_chain: u16, addr_seed: u8) -> BalanceRow {
    let mut token_address = [0u8; 32];
    token_address[31] = addr_seed;
    BalanceRow {
        chain: ChainId(chain),
        token_chain: ChainId(token_chain),
        token_address: Address(token_address),
        balance: Uint256::ZERO,
    }
}

#[test]
fn empty_input_yields_nothing() {
    let batcher = Batcher::new(std::iter::empty::<SnapshotRow>());
    assert_eq!(batcher.count(), 0);
}

#[test]
fn single_transfer_yields_one_noreplay_batch_of_one() {
    let input = vec![SnapshotRow::Transfer(make_transfer(1, 0xAA, 5))];
    let batches: Vec<TxBatch> = Batcher::new(input.into_iter()).collect();
    assert_eq!(batches.len(), 1);
    match &batches[0] {
        TxBatch::BackfillNoReplay(entries) => assert_eq!(entries.len(), 1),
        other => panic!("expected BackfillNoReplay, got {other:?}"),
    }
}

#[test]
fn same_emitter_transfers_pack_up_to_max() {
    let emitter = 0xAA;
    let input: Vec<SnapshotRow> = (0..MAX_NOREPLAY_ENTRIES_PER_BATCH as u64)
        .map(|i| SnapshotRow::Transfer(make_transfer(1, emitter, i)))
        .collect();
    let batches: Vec<TxBatch> = Batcher::new(input.into_iter()).collect();
    assert_eq!(batches.len(), 1);
    if let TxBatch::BackfillNoReplay(entries) = &batches[0] {
        assert_eq!(entries.len(), MAX_NOREPLAY_ENTRIES_PER_BATCH);
        // Sequences preserved in order
        for (i, e) in entries.iter().enumerate() {
            assert_eq!(e.sequence, Sequence(i as u64));
        }
    } else {
        panic!("expected BackfillNoReplay");
    }
}

#[test]
fn same_emitter_overflow_splits_at_max() {
    // 19 entries from one emitter → batch 18 + batch 1
    let count = MAX_NOREPLAY_ENTRIES_PER_BATCH + 1;
    let input: Vec<SnapshotRow> = (0..count as u64)
        .map(|i| SnapshotRow::Transfer(make_transfer(1, 0xAA, i)))
        .collect();
    let batches: Vec<TxBatch> = Batcher::new(input.into_iter()).collect();
    assert_eq!(batches.len(), 2);
    if let TxBatch::BackfillNoReplay(first) = &batches[0] {
        assert_eq!(first.len(), MAX_NOREPLAY_ENTRIES_PER_BATCH);
    }
    if let TxBatch::BackfillNoReplay(second) = &batches[1] {
        assert_eq!(second.len(), 1);
        assert_eq!(
            second[0].sequence,
            Sequence(MAX_NOREPLAY_ENTRIES_PER_BATCH as u64)
        );
    }
}

#[test]
fn different_emitters_each_become_own_batch() {
    // 5 from emitter A, 5 from emitter B — should not merge into one batch of 10
    let mut input = vec![];
    for i in 0..5 {
        input.push(SnapshotRow::Transfer(make_transfer(1, 0xAA, i)));
    }
    for i in 0..5 {
        input.push(SnapshotRow::Transfer(make_transfer(1, 0xBB, i)));
    }
    let batches: Vec<TxBatch> = Batcher::new(input.into_iter()).collect();
    assert_eq!(batches.len(), 2);
    if let TxBatch::BackfillNoReplay(a) = &batches[0] {
        assert_eq!(a.len(), 5);
        assert_eq!(a[0].emitter.0[31], 0xAA);
    }
    if let TxBatch::BackfillNoReplay(b) = &batches[1] {
        assert_eq!(b.len(), 5);
        assert_eq!(b[0].emitter.0[31], 0xBB);
    }
}

#[test]
fn different_chains_same_emitter_seed_split() {
    // Same emitter bytes but different chain IDs → different groups
    let input = vec![
        SnapshotRow::Transfer(make_transfer(1, 0xAA, 0)),
        SnapshotRow::Transfer(make_transfer(2, 0xAA, 0)),
    ];
    let batches: Vec<TxBatch> = Batcher::new(input.into_iter()).collect();
    assert_eq!(batches.len(), 2);
}

#[test]
fn balances_pack_up_to_max() {
    let input: Vec<SnapshotRow> = (0..MAX_BALANCE_ENTRIES_PER_BATCH as u8)
        .map(|i| SnapshotRow::Balance(make_balance(1, 2, i)))
        .collect();
    let batches: Vec<TxBatch> = Batcher::new(input.into_iter()).collect();
    assert_eq!(batches.len(), 1);
    if let TxBatch::BackfillBalance(entries) = &batches[0] {
        assert_eq!(entries.len(), MAX_BALANCE_ENTRIES_PER_BATCH);
    } else {
        panic!("expected BackfillBalance");
    }
}

#[test]
fn balances_overflow_splits_at_max() {
    let count = MAX_BALANCE_ENTRIES_PER_BATCH + 2;
    let input: Vec<SnapshotRow> = (0..count as u8)
        .map(|i| SnapshotRow::Balance(make_balance(1, 2, i)))
        .collect();
    let batches: Vec<TxBatch> = Batcher::new(input.into_iter()).collect();
    assert_eq!(batches.len(), 2);
    if let TxBatch::BackfillBalance(first) = &batches[0] {
        assert_eq!(first.len(), MAX_BALANCE_ENTRIES_PER_BATCH);
    }
    if let TxBatch::BackfillBalance(second) = &batches[1] {
        assert_eq!(second.len(), 2);
    }
}

#[test]
fn balances_sorted_into_handler_order_across_batches() {
    // The snapshot groups accounts by `(chain, token_address, token_chain)`,
    // which does NOT match the `BackfillBalance` handler's required order
    // `(chain, token_chain, token_address)`. Feed one chain's accounts with
    // descending `token_chain` spanning more than one batch and assert every
    // emitted batch — and the global sequence — is strictly ascending by the
    // handler key. Regression: a greedy consecutive pack emitted a
    // non-ascending batch the program rejected with `InvalidInstructionData`.
    let n = MAX_BALANCE_ENTRIES_PER_BATCH + 1; // force a split
    let input: Vec<SnapshotRow> = (0..n as u16)
        .rev() // descending token_chain: n-1, n-2, …, 0
        .map(|tc| SnapshotRow::Balance(make_balance(1, tc, 0)))
        .collect();
    let batches: Vec<TxBatch> = Batcher::new(input.into_iter()).collect();

    let mut flat: Vec<BalanceKey> = Vec::new();
    for batch in &batches {
        let TxBatch::BackfillBalance(entries) = batch else {
            panic!("expected BackfillBalance");
        };
        for w in entries.windows(2) {
            let (a, b) = (w[0].key(), w[1].key());
            assert!(a < b, "batch not strictly ascending: {a:?} !< {b:?}");
        }
        flat.extend(entries.iter().map(BalanceRow::key));
    }
    assert_eq!(flat.len(), n, "all balances must be emitted exactly once");
    for w in flat.windows(2) {
        assert!(
            w[0] < w[1],
            "global order not ascending: {:?} !< {:?}",
            w[0],
            w[1]
        );
    }
}

#[test]
fn kind_transition_closes_current_batch() {
    // 3 accounts, then 1 transfer → 2 batches, not 1
    let input = vec![
        SnapshotRow::Balance(make_balance(1, 2, 0)),
        SnapshotRow::Balance(make_balance(1, 2, 1)),
        SnapshotRow::Balance(make_balance(1, 2, 2)),
        SnapshotRow::Transfer(make_transfer(1, 0xAA, 0)),
    ];
    let batches: Vec<TxBatch> = Batcher::new(input.into_iter()).collect();
    assert_eq!(batches.len(), 2);
    matches!(batches[0], TxBatch::BackfillBalance(_));
    matches!(batches[1], TxBatch::BackfillNoReplay(_));
}

fn make_modification(sequence: u64) -> ModifyBalanceRow {
    ModifyBalanceRow {
        sequence: Sequence(sequence),
        chain_id: ChainId(2),
        token_chain: ChainId(2),
        token_address: Address([0u8; 32]),
        amount: Uint256([0u8; 32]),
        reason: [0u8; 32],
        modify_kind: ModificationKind::Add,
    }
}

#[test]
fn modifications_sorted_by_sequence_and_split_at_max() {
    let count = MAX_MODIFY_BALANCE_ENTRIES_PER_BATCH as u64 + 1;
    let input: Vec<SnapshotRow> = (0..count)
        .rev()
        .map(|s| SnapshotRow::ModifyBalance(make_modification(s + 1)))
        .collect();
    let batches: Vec<TxBatch> = Batcher::new(input.into_iter()).collect();
    assert_eq!(batches.len(), 2);
    let TxBatch::BackfillModifyBalance(first) = &batches[0] else {
        panic!("expected BackfillModifyBalance, got {:?}", batches[0]);
    };
    assert_eq!(first.len(), MAX_MODIFY_BALANCE_ENTRIES_PER_BATCH);
    assert!(first.windows(2).all(|w| w[0].sequence < w[1].sequence));
    assert!(matches!(&batches[1], TxBatch::BackfillModifyBalance(e) if e.len() == 1));
    for batch in &batches {
        svm_accountant_backfill::tx_builder::build_ix(&target(Accountant::Wtt), batch)
            .expect("valid batch");
    }
}

#[test]
fn registrations_yield_chain_registration_batch() {
    let r = ChainRegistrationRow {
        chain: ChainId(2),
        registered_emitter: Address([0u8; 32]),
        sequence: Sequence(1),
    };
    let batches: Vec<TxBatch> =
        Batcher::new(std::iter::once(SnapshotRow::TokenBridgeRegistration(r))).collect();
    assert_eq!(batches.len(), 1);
    assert!(matches!(&batches[0], TxBatch::BackfillChainRegistration(e) if e.len() == 1));
}

#[test]
fn snapshot_order_walkthrough() {
    // Mirrors the real snapshot's sort order:
    // accounts → modifications → registrations → transfers
    let input = vec![
        SnapshotRow::Balance(make_balance(1, 2, 0)),
        SnapshotRow::Balance(make_balance(1, 2, 1)),
        SnapshotRow::ModifyBalance(ModifyBalanceRow {
            sequence: Sequence(1),
            chain_id: ChainId(0),
            token_chain: ChainId(0),
            token_address: Address([0u8; 32]),
            amount: Uint256([0u8; 32]),
            reason: [0u8; 32],
            modify_kind: ModificationKind::Subtract,
        }),
        SnapshotRow::TokenBridgeRegistration(ChainRegistrationRow {
            chain: ChainId(2),
            registered_emitter: Address([0u8; 32]),
            sequence: Sequence(1),
        }),
        SnapshotRow::Transfer(make_transfer(1, 0xAA, 5)),
        SnapshotRow::Transfer(make_transfer(1, 0xAA, 6)),
        SnapshotRow::Transfer(make_transfer(1, 0xBB, 5)),
    ];
    let batches: Vec<TxBatch> = Batcher::new(input.into_iter()).collect();
    // Expect: 1 BackfillBalance (2 accs) + 1 ModifyBalance + 1 ChainRegistration
    //         + 1 BackfillNoReplay (2 transfers from emitter AA)
    //         + 1 BackfillNoReplay (1 transfer from emitter BB)
    // = 5 batches total
    assert_eq!(batches.len(), 5);
    assert!(matches!(batches[0], TxBatch::BackfillBalance(ref e) if e.len() == 2));
    assert!(matches!(batches[1], TxBatch::BackfillModifyBalance(_)));
    assert!(matches!(batches[2], TxBatch::BackfillChainRegistration(_)));
    assert!(matches!(batches[3], TxBatch::BackfillNoReplay(ref e) if e.len() == 2));
    assert!(matches!(batches[4], TxBatch::BackfillNoReplay(ref e) if e.len() == 1));
}

// ============================================================================
// NTT map caps
// ============================================================================

#[test]
fn ntt_caps_match_derivation() {
    // Entry budget = 8 * (32 meta + 68 data) = 800 bytes (anchored to the WTT
    // Balance cap). Each cap is 800 / (metas * 32 + per-entry-data-bytes).
    assert_eq!(MAX_CHAIN_REGISTRATION_ENTRIES_PER_BATCH, 7); // 800 / (2 * 32 + 42)
    assert_eq!(MAX_MODIFY_BALANCE_ENTRIES_PER_BATCH, 5); // 800 / (32 + 109)
    assert_eq!(MAX_TRANSCEIVER_HUB_ENTRIES_PER_BATCH, 8); // 800 / (32 + 68)
    assert_eq!(MAX_TRANSCEIVER_PEER_ENTRIES_PER_BATCH, 8); // 800 / (32 + 68)
}

// ----- relayer registration -----

#[test]
fn relayer_registrations_pack_up_to_max() {
    let input: Vec<SnapshotRow> = (0..MAX_CHAIN_REGISTRATION_ENTRIES_PER_BATCH as u16)
        .map(|c| SnapshotRow::RelayerChainRegistration(make_relayer(c + 1)))
        .collect();
    let batches: Vec<TxBatch> = Batcher::new(input.into_iter()).collect();
    assert_eq!(batches.len(), 1);
    match &batches[0] {
        TxBatch::BackfillChainRegistration(e) => {
            assert_eq!(e.len(), MAX_CHAIN_REGISTRATION_ENTRIES_PER_BATCH);
            // strictly ascending by chain
            for w in e.windows(2) {
                assert!(w[0].chain < w[1].chain);
            }
        }
        other => panic!("expected BackfillChainRegistration, got {other:?}"),
    }
}

#[test]
fn relayer_registrations_overflow_splits_at_max() {
    let count = MAX_CHAIN_REGISTRATION_ENTRIES_PER_BATCH + 1;
    let input: Vec<SnapshotRow> = (0..count as u16)
        .map(|c| SnapshotRow::RelayerChainRegistration(make_relayer(c + 1)))
        .collect();
    let batches: Vec<TxBatch> = Batcher::new(input.into_iter()).collect();
    assert_eq!(batches.len(), 2);
    assert!(matches!(
        &batches[0],
        TxBatch::BackfillChainRegistration(e) if e.len() == MAX_CHAIN_REGISTRATION_ENTRIES_PER_BATCH
    ));
    assert!(matches!(
        &batches[1],
        TxBatch::BackfillChainRegistration(e) if e.len() == 1
    ));
}

#[test]
fn relayer_registrations_sorted_and_duplicate_chain_rejected_before_send() {
    // The run is sorted into handler order; a duplicate chain breaks
    // strict-ascending, and `build_ix` rejects the batch before any send.
    let sorted: Vec<TxBatch> = Batcher::new(
        vec![
            SnapshotRow::RelayerChainRegistration(make_relayer(5)),
            SnapshotRow::RelayerChainRegistration(make_relayer(3)),
        ]
        .into_iter(),
    )
    .collect();
    assert_eq!(sorted.len(), 1);
    assert!(matches!(
        &sorted[0],
        TxBatch::BackfillChainRegistration(e) if e[0].chain == ChainId(3) && e[1].chain == ChainId(5)
    ));
    svm_accountant_backfill::tx_builder::build_ix(&target(Accountant::Ntt), &sorted[0])
        .expect("ascending batch");

    let duplicate: Vec<TxBatch> = Batcher::new(
        vec![
            SnapshotRow::RelayerChainRegistration(make_relayer(3)),
            SnapshotRow::RelayerChainRegistration(make_relayer(3)), // not > 3
        ]
        .into_iter(),
    )
    .collect();
    assert_eq!(duplicate.len(), 1);
    assert!(
        svm_accountant_backfill::tx_builder::build_ix(&target(Accountant::Ntt), &duplicate[0])
            .is_err(),
        "duplicate chain must fail the definitions parser"
    );
}

// ----- transceiver hub -----

#[test]
fn transceiver_hubs_pack_up_to_max() {
    let input: Vec<SnapshotRow> = (0..MAX_TRANSCEIVER_HUB_ENTRIES_PER_BATCH as u8)
        .map(|i| SnapshotRow::TransceiverHub(make_hub(2, i + 1)))
        .collect();
    let batches: Vec<TxBatch> = Batcher::new(input.into_iter()).collect();
    assert_eq!(batches.len(), 1);
    match &batches[0] {
        TxBatch::BackfillTransceiverHub(e) => {
            assert_eq!(e.len(), MAX_TRANSCEIVER_HUB_ENTRIES_PER_BATCH);
            for w in e.windows(2) {
                assert!((w[0].chain, w[0].address) < (w[1].chain, w[1].address));
            }
        }
        other => panic!("expected BackfillTransceiverHub, got {other:?}"),
    }
}

#[test]
fn transceiver_hubs_overflow_splits_at_max() {
    let count = MAX_TRANSCEIVER_HUB_ENTRIES_PER_BATCH + 2;
    let input: Vec<SnapshotRow> = (0..count as u8)
        .map(|i| SnapshotRow::TransceiverHub(make_hub(2, i + 1)))
        .collect();
    let batches: Vec<TxBatch> = Batcher::new(input.into_iter()).collect();
    assert_eq!(batches.len(), 2);
    assert!(matches!(
        &batches[0],
        TxBatch::BackfillTransceiverHub(e) if e.len() == MAX_TRANSCEIVER_HUB_ENTRIES_PER_BATCH
    ));
    assert!(matches!(
        &batches[1],
        TxBatch::BackfillTransceiverHub(e) if e.len() == 2
    ));
}

#[test]
fn transceiver_hubs_sorted_and_duplicate_key_rejected_before_send() {
    // The run is sorted into one batch; an equal (chain, address) breaks
    // strict-ascending, and `check_batch_order` rejects it before any send.
    let input = vec![
        SnapshotRow::TransceiverHub(make_hub(2, 5)),
        SnapshotRow::TransceiverHub(make_hub(2, 5)),
    ];
    let batches: Vec<TxBatch> = Batcher::new(input.into_iter()).collect();
    assert_eq!(batches.len(), 1);
    assert!(svm_accountant_backfill::batcher::check_batch_order(&batches).is_err());
}

// ----- transceiver peer -----

#[test]
fn transceiver_peers_pack_up_to_max() {
    // Vary dest_chain to keep the triple strictly ascending.
    let input: Vec<SnapshotRow> = (0..MAX_TRANSCEIVER_PEER_ENTRIES_PER_BATCH as u16)
        .map(|i| SnapshotRow::TransceiverPeer(make_peer(2, 7, i + 1)))
        .collect();
    let batches: Vec<TxBatch> = Batcher::new(input.into_iter()).collect();
    assert_eq!(batches.len(), 1);
    match &batches[0] {
        TxBatch::BackfillTransceiverPeer(e) => {
            assert_eq!(e.len(), MAX_TRANSCEIVER_PEER_ENTRIES_PER_BATCH);
            for w in e.windows(2) {
                assert!(
                    (w[0].chain, w[0].address, w[0].dest_chain)
                        < (w[1].chain, w[1].address, w[1].dest_chain)
                );
            }
        }
        other => panic!("expected BackfillTransceiverPeer, got {other:?}"),
    }
}

#[test]
fn transceiver_peers_overflow_splits_at_max() {
    let count = MAX_TRANSCEIVER_PEER_ENTRIES_PER_BATCH + 1;
    let input: Vec<SnapshotRow> = (0..count as u16)
        .map(|i| SnapshotRow::TransceiverPeer(make_peer(2, 7, i + 1)))
        .collect();
    let batches: Vec<TxBatch> = Batcher::new(input.into_iter()).collect();
    assert_eq!(batches.len(), 2);
    assert!(matches!(
        &batches[0],
        TxBatch::BackfillTransceiverPeer(e) if e.len() == MAX_TRANSCEIVER_PEER_ENTRIES_PER_BATCH
    ));
    assert!(matches!(
        &batches[1],
        TxBatch::BackfillTransceiverPeer(e) if e.len() == 1
    ));
}

#[test]
fn transceiver_peers_sorted_and_duplicate_key_rejected_before_send() {
    let input = vec![
        SnapshotRow::TransceiverPeer(make_peer(2, 7, 4)),
        SnapshotRow::TransceiverPeer(make_peer(2, 7, 4)),
    ];
    let batches: Vec<TxBatch> = Batcher::new(input.into_iter()).collect();
    assert_eq!(batches.len(), 1);
    assert!(svm_accountant_backfill::batcher::check_batch_order(&batches).is_err());
}

// ============================================================================
// Bucket-boundary-straddling wire-size edge the module doc warns about: 18
// same-emitter entries, one per noreplay bucket, is the maximum possible
// bucket span for what used to be an 18-entry batch cap.
// ============================================================================

#[test]
fn eighteen_entries_in_eighteen_buckets_split_below_packet_limit() {
    use global_accountant_definitions::NOREPLAY_BITS_PER_BUCKET;
    use solana_hash::Hash;
    use solana_keypair::Keypair;
    use solana_signer::Signer;
    use solana_transaction::Transaction;
    use svm_accountant_backfill::tx_builder::build_backfill_noreplay_ix;
    use svm_accountant_backfill::wire::PACKET_DATA_SIZE;

    // 18 entries, same (chain, emitter), each in its own bucket — the
    // maximum possible bucket span for an 18-entry same-emitter batch.
    let input: Vec<SnapshotRow> = (0..MAX_NOREPLAY_ENTRIES_PER_BATCH as u64)
        .map(|i| {
            let mut t = make_transfer(1, 0xAA, i * NOREPLAY_BITS_PER_BUCKET);
            t.digest.0[0] = i as u8; // distinguish entries
            SnapshotRow::Transfer(t)
        })
        .collect();
    let batches: Vec<TxBatch> = Batcher::new(input.into_iter()).collect();

    // Bucket-aware batcher must split this batch instead of packing all 18
    // entries into one batch.
    assert!(
        batches.len() > 1,
        "expected the bucket-aware batcher to split the worst-case batch \
         into more than one batch, got {} batch(s)",
        batches.len()
    );

    let mut total_entries = 0usize;
    let payer = Keypair::new_from_array([1u8; 32]); // WTT backfill authority
    for (i, batch) in batches.iter().enumerate() {
        let TxBatch::BackfillNoReplay(transfers) = batch else {
            panic!("expected BackfillNoReplay, got {batch:?}");
        };
        total_entries += transfers.len();

        // Every emitted batch's real, signed wire size must stay within
        // the packet limit.
        let ix = build_backfill_noreplay_ix(&target(Accountant::Wtt), transfers);
        let tx = Transaction::new_signed_with_payer(
            std::slice::from_ref(&ix),
            Some(&payer.pubkey()),
            &[&payer],
            Hash::new_from_array([7u8; 32]),
        );
        let wire_size = bincode::serialize(&tx).expect("serialize tx").len();
        eprintln!(
            "[wire-size] batch {i}: {} entries, {} accounts, {wire_size} bytes \
             (PACKET_DATA_SIZE = {PACKET_DATA_SIZE})",
            transfers.len(),
            ix.accounts.len(),
        );
        assert!(
            wire_size <= PACKET_DATA_SIZE,
            "GAP WOULD BE REINTRODUCED: batch {i} ({} entries) serializes to \
             {wire_size} bytes, over PACKET_DATA_SIZE ({PACKET_DATA_SIZE}) — \
             the bucket-aware bound in collect_transfers is not holding",
            transfers.len()
        );
    }
    assert_eq!(
        total_entries, MAX_NOREPLAY_ENTRIES_PER_BATCH,
        "every input entry must still be emitted exactly once, just spread \
         across more batches"
    );

    // Boundary check: packing all 18 entries into a single batch must
    // still overflow the packet limit.
    let target = target(Accountant::Wtt);
    let all: Vec<TransferRow> = batches
        .iter()
        .flat_map(|c| match c {
            TxBatch::BackfillNoReplay(e) => e.clone(),
            _ => unreachable!(),
        })
        .collect();
    let unified_ix = build_backfill_noreplay_ix(&target, &all);
    let unified_tx = Transaction::new_signed_with_payer(
        std::slice::from_ref(&unified_ix),
        Some(&payer.pubkey()),
        &[&payer],
        Hash::new_from_array([7u8; 32]),
    );
    let unified_wire_size = bincode::serialize(&unified_tx).expect("serialize tx").len();
    assert_eq!(
        unified_wire_size, 1621,
        "if this changes, the wire-size math changed — re-verify against \
         PACKET_DATA_SIZE (1232) and update this pinned reference value"
    );
    assert!(
        unified_wire_size > PACKET_DATA_SIZE,
        "the original gap (all 18 packed as one batch) must still measure \
         as an overflow — otherwise this test no longer proves anything"
    );
}

#[test]
fn ntt_kind_transition_closes_batch() {
    // relayer → hub → peer: three distinct batches, no cross-kind packing.
    let input = vec![
        SnapshotRow::RelayerChainRegistration(make_relayer(1)),
        SnapshotRow::TransceiverHub(make_hub(2, 1)),
        SnapshotRow::TransceiverPeer(make_peer(2, 1, 3)),
    ];
    let batches: Vec<TxBatch> = Batcher::new(input.into_iter()).collect();
    assert_eq!(batches.len(), 3);
    assert!(matches!(batches[0], TxBatch::BackfillChainRegistration(_)));
    assert!(matches!(batches[1], TxBatch::BackfillTransceiverHub(_)));
    assert!(matches!(batches[2], TxBatch::BackfillTransceiverPeer(_)));
}
