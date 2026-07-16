//! Integration tests for the catalogue → chunk planner.

use ga_backfill::catalogue::{
    AccountRecord, ModificationRecord, ModifyKind, Record, RegistrationRecord,
    RelayerChainRegistrationRecord, TransceiverHubRecord, TransceiverPeerRecord, TransferRecord,
};
use ga_backfill::chunker::{
    ChunkPlan, Chunker, MAX_BALANCE_ENTRIES_PER_CHUNK, MAX_NOREPLAY_ENTRIES_PER_CHUNK,
    MAX_RELAYER_REGISTRATION_ENTRIES_PER_CHUNK, MAX_TRANSCEIVER_HUB_ENTRIES_PER_CHUNK,
    MAX_TRANSCEIVER_PEER_ENTRIES_PER_CHUNK,
};

fn make_relayer(chain: u16) -> RelayerChainRegistrationRecord {
    let mut registered_emitter = [0u8; 32];
    registered_emitter[31] = (chain & 0xff) as u8;
    RelayerChainRegistrationRecord {
        chain,
        registered_emitter,
    }
}

fn make_hub(chain: u16, addr_seed: u8) -> TransceiverHubRecord {
    let mut address = [0u8; 32];
    address[31] = addr_seed;
    TransceiverHubRecord {
        chain,
        address,
        hub_chain: 1,
        hub_address: [0u8; 32],
    }
}

fn make_peer(chain: u16, addr_seed: u8, dest_chain: u16) -> TransceiverPeerRecord {
    let mut address = [0u8; 32];
    address[31] = addr_seed;
    TransceiverPeerRecord {
        chain,
        address,
        dest_chain,
        peer_address: [0u8; 32],
    }
}

fn make_transfer(chain: u16, emitter_seed: u8, sequence: u64) -> TransferRecord {
    let mut emitter = [0u8; 32];
    emitter[31] = emitter_seed;
    let mut digest = [0u8; 32];
    digest[31] = (sequence & 0xff) as u8;
    TransferRecord {
        chain,
        emitter,
        sequence,
        digest,
        amount: [0u8; 32],
        token_chain: 1,
        token_address: [0u8; 32],
        recipient_chain: 2,
    }
}

fn make_account(chain: u16, token_chain: u16, addr_seed: u8) -> AccountRecord {
    let mut token_address = [0u8; 32];
    token_address[31] = addr_seed;
    AccountRecord {
        chain,
        token_chain,
        token_address,
        balance: [0u8; 32],
    }
}

#[test]
fn empty_input_yields_nothing() {
    let chunker = Chunker::new(std::iter::empty::<Record>());
    assert_eq!(chunker.count(), 0);
}

#[test]
fn single_transfer_yields_one_noreplay_chunk_of_one() {
    let input = vec![Record::Transfer(make_transfer(1, 0xAA, 5))];
    let chunks: Vec<ChunkPlan> = Chunker::new(input.into_iter()).collect();
    assert_eq!(chunks.len(), 1);
    match &chunks[0] {
        ChunkPlan::BackfillNoReplay(entries) => assert_eq!(entries.len(), 1),
        other => panic!("expected BackfillNoReplay, got {other:?}"),
    }
}

#[test]
fn same_emitter_transfers_pack_up_to_max() {
    let emitter = 0xAA;
    let input: Vec<Record> = (0..MAX_NOREPLAY_ENTRIES_PER_CHUNK as u64)
        .map(|i| Record::Transfer(make_transfer(1, emitter, i)))
        .collect();
    let chunks: Vec<ChunkPlan> = Chunker::new(input.into_iter()).collect();
    assert_eq!(chunks.len(), 1);
    if let ChunkPlan::BackfillNoReplay(entries) = &chunks[0] {
        assert_eq!(entries.len(), MAX_NOREPLAY_ENTRIES_PER_CHUNK);
        // Sequences preserved in order
        for (i, e) in entries.iter().enumerate() {
            assert_eq!(e.sequence, i as u64);
        }
    } else {
        panic!("expected BackfillNoReplay");
    }
}

#[test]
fn same_emitter_overflow_splits_at_max() {
    // 19 entries from one emitter → chunk 18 + chunk 1
    let count = MAX_NOREPLAY_ENTRIES_PER_CHUNK + 1;
    let input: Vec<Record> = (0..count as u64)
        .map(|i| Record::Transfer(make_transfer(1, 0xAA, i)))
        .collect();
    let chunks: Vec<ChunkPlan> = Chunker::new(input.into_iter()).collect();
    assert_eq!(chunks.len(), 2);
    if let ChunkPlan::BackfillNoReplay(first) = &chunks[0] {
        assert_eq!(first.len(), MAX_NOREPLAY_ENTRIES_PER_CHUNK);
    }
    if let ChunkPlan::BackfillNoReplay(second) = &chunks[1] {
        assert_eq!(second.len(), 1);
        assert_eq!(second[0].sequence, MAX_NOREPLAY_ENTRIES_PER_CHUNK as u64);
    }
}

#[test]
fn different_emitters_each_become_own_chunk() {
    // 5 from emitter A, 5 from emitter B — should not merge into one chunk of 10
    let mut input = vec![];
    for i in 0..5 {
        input.push(Record::Transfer(make_transfer(1, 0xAA, i)));
    }
    for i in 0..5 {
        input.push(Record::Transfer(make_transfer(1, 0xBB, i)));
    }
    let chunks: Vec<ChunkPlan> = Chunker::new(input.into_iter()).collect();
    assert_eq!(chunks.len(), 2);
    if let ChunkPlan::BackfillNoReplay(a) = &chunks[0] {
        assert_eq!(a.len(), 5);
        assert_eq!(a[0].emitter[31], 0xAA);
    }
    if let ChunkPlan::BackfillNoReplay(b) = &chunks[1] {
        assert_eq!(b.len(), 5);
        assert_eq!(b[0].emitter[31], 0xBB);
    }
}

#[test]
fn different_chains_same_emitter_seed_split() {
    // Same emitter bytes but different chain IDs → different groups
    let input = vec![
        Record::Transfer(make_transfer(1, 0xAA, 0)),
        Record::Transfer(make_transfer(2, 0xAA, 0)),
    ];
    let chunks: Vec<ChunkPlan> = Chunker::new(input.into_iter()).collect();
    assert_eq!(chunks.len(), 2);
}

#[test]
fn accounts_pack_up_to_max() {
    let input: Vec<Record> = (0..MAX_BALANCE_ENTRIES_PER_CHUNK as u8)
        .map(|i| Record::Account(make_account(1, 2, i)))
        .collect();
    let chunks: Vec<ChunkPlan> = Chunker::new(input.into_iter()).collect();
    assert_eq!(chunks.len(), 1);
    if let ChunkPlan::BackfillBalance(entries) = &chunks[0] {
        assert_eq!(entries.len(), MAX_BALANCE_ENTRIES_PER_CHUNK);
    } else {
        panic!("expected BackfillBalance");
    }
}

#[test]
fn accounts_overflow_splits_at_max() {
    let count = MAX_BALANCE_ENTRIES_PER_CHUNK + 2;
    let input: Vec<Record> = (0..count as u8)
        .map(|i| Record::Account(make_account(1, 2, i)))
        .collect();
    let chunks: Vec<ChunkPlan> = Chunker::new(input.into_iter()).collect();
    assert_eq!(chunks.len(), 2);
    if let ChunkPlan::BackfillBalance(first) = &chunks[0] {
        assert_eq!(first.len(), MAX_BALANCE_ENTRIES_PER_CHUNK);
    }
    if let ChunkPlan::BackfillBalance(second) = &chunks[1] {
        assert_eq!(second.len(), 2);
    }
}

#[test]
fn accounts_sorted_into_handler_order_across_chunks() {
    // The catalogue groups accounts by `(chain, token_address, token_chain)`,
    // which does NOT match the `BackfillBalance` handler's required order
    // `(chain, token_chain, token_address)`. Feed one chain's accounts with
    // descending `token_chain` spanning more than one chunk and assert every
    // emitted chunk — and the global sequence — is strictly ascending by the
    // handler key. Regression: a greedy consecutive pack emitted a
    // non-ascending chunk the program rejected with `InvalidInstructionData`.
    let n = MAX_BALANCE_ENTRIES_PER_CHUNK + 1; // force a split
    let input: Vec<Record> = (0..n as u16)
        .rev() // descending token_chain: n-1, n-2, …, 0
        .map(|tc| Record::Account(make_account(1, tc, 0)))
        .collect();
    let chunks: Vec<ChunkPlan> = Chunker::new(input.into_iter()).collect();

    let mut flat: Vec<(u16, u16, [u8; 32])> = Vec::new();
    for chunk in &chunks {
        let ChunkPlan::BackfillBalance(entries) = chunk else {
            panic!("expected BackfillBalance");
        };
        for w in entries.windows(2) {
            let a = (w[0].chain, w[0].token_chain, w[0].token_address);
            let b = (w[1].chain, w[1].token_chain, w[1].token_address);
            assert!(a < b, "chunk not strictly ascending: {a:?} !< {b:?}");
        }
        flat.extend(entries.iter().map(|e| (e.chain, e.token_chain, e.token_address)));
    }
    assert_eq!(flat.len(), n, "all accounts must be emitted exactly once");
    for w in flat.windows(2) {
        assert!(w[0] < w[1], "global order not ascending: {:?} !< {:?}", w[0], w[1]);
    }
}

#[test]
fn kind_transition_closes_current_chunk() {
    // 3 accounts, then 1 transfer → 2 chunks, not 1
    let input = vec![
        Record::Account(make_account(1, 2, 0)),
        Record::Account(make_account(1, 2, 1)),
        Record::Account(make_account(1, 2, 2)),
        Record::Transfer(make_transfer(1, 0xAA, 0)),
    ];
    let chunks: Vec<ChunkPlan> = Chunker::new(input.into_iter()).collect();
    assert_eq!(chunks.len(), 2);
    matches!(chunks[0], ChunkPlan::BackfillBalance(_));
    matches!(chunks[1], ChunkPlan::BackfillNoReplay(_));
}

#[test]
fn modifications_yield_deferred_variant() {
    let m = ModificationRecord {
        sequence: 1,
        chain_id: 2,
        token_chain: 2,
        token_address: [0u8; 32],
        amount: [0u8; 32],
        reason: "test".into(),
        modify_kind: ModifyKind::Add,
    };
    let chunks: Vec<ChunkPlan> = Chunker::new(std::iter::once(Record::Modification(m))).collect();
    assert_eq!(chunks.len(), 1);
    assert!(matches!(chunks[0], ChunkPlan::DeferredModification(_)));
}

#[test]
fn registrations_yield_deferred_variant() {
    let r = RegistrationRecord {
        chain: 2,
        registered_emitter: [0u8; 32],
    };
    let chunks: Vec<ChunkPlan> = Chunker::new(std::iter::once(Record::Registration(r))).collect();
    assert_eq!(chunks.len(), 1);
    assert!(matches!(chunks[0], ChunkPlan::DeferredRegistration(_)));
}

#[test]
fn catalogue_order_walkthrough() {
    // Mirrors the real catalogue's sort order:
    // accounts → modifications → registrations → transfers
    let input = vec![
        Record::Account(make_account(1, 2, 0)),
        Record::Account(make_account(1, 2, 1)),
        Record::Modification(ModificationRecord {
            sequence: 1,
            chain_id: 0,
            token_chain: 0,
            token_address: [0u8; 32],
            amount: [0u8; 32],
            reason: "x".into(),
            modify_kind: ModifyKind::Subtract,
        }),
        Record::Registration(RegistrationRecord {
            chain: 2,
            registered_emitter: [0u8; 32],
        }),
        Record::Transfer(make_transfer(1, 0xAA, 5)),
        Record::Transfer(make_transfer(1, 0xAA, 6)),
        Record::Transfer(make_transfer(1, 0xBB, 5)),
    ];
    let chunks: Vec<ChunkPlan> = Chunker::new(input.into_iter()).collect();
    // Expect: 1 BackfillBalance (2 accs) + 1 DeferredMod + 1 DeferredReg
    //         + 1 BackfillNoReplay (2 transfers from emitter AA)
    //         + 1 BackfillNoReplay (1 transfer from emitter BB)
    // = 5 chunks total
    assert_eq!(chunks.len(), 5);
    assert!(matches!(chunks[0], ChunkPlan::BackfillBalance(ref e) if e.len() == 2));
    assert!(matches!(chunks[1], ChunkPlan::DeferredModification(_)));
    assert!(matches!(chunks[2], ChunkPlan::DeferredRegistration(_)));
    assert!(matches!(chunks[3], ChunkPlan::BackfillNoReplay(ref e) if e.len() == 2));
    assert!(matches!(chunks[4], ChunkPlan::BackfillNoReplay(ref e) if e.len() == 1));
}

// ============================================================================
// NTT map caps
// ============================================================================

#[test]
fn ntt_caps_match_derivation() {
    // Entry budget = 8 * (32 meta + 68 data) = 800 bytes (anchored to the WTT
    // Balance cap). Each NTT cap is 800 / (32 + per-entry-data-bytes).
    assert_eq!(MAX_RELAYER_REGISTRATION_ENTRIES_PER_CHUNK, 12); // 800 / (32 + 34)
    assert_eq!(MAX_TRANSCEIVER_HUB_ENTRIES_PER_CHUNK, 8); // 800 / (32 + 68)
    assert_eq!(MAX_TRANSCEIVER_PEER_ENTRIES_PER_CHUNK, 8); // 800 / (32 + 68)
}

// ----- relayer registration -----

#[test]
fn relayer_registrations_pack_up_to_max() {
    let input: Vec<Record> = (0..MAX_RELAYER_REGISTRATION_ENTRIES_PER_CHUNK as u16)
        .map(|c| Record::RelayerChainRegistration(make_relayer(c + 1)))
        .collect();
    let chunks: Vec<ChunkPlan> = Chunker::new(input.into_iter()).collect();
    assert_eq!(chunks.len(), 1);
    match &chunks[0] {
        ChunkPlan::BackfillRelayerRegistration(e) => {
            assert_eq!(e.len(), MAX_RELAYER_REGISTRATION_ENTRIES_PER_CHUNK);
            // strictly ascending by chain
            for w in e.windows(2) {
                assert!(w[0].chain < w[1].chain);
            }
        }
        other => panic!("expected BackfillRelayerRegistration, got {other:?}"),
    }
}

#[test]
fn relayer_registrations_overflow_splits_at_max() {
    let count = MAX_RELAYER_REGISTRATION_ENTRIES_PER_CHUNK + 1;
    let input: Vec<Record> = (0..count as u16)
        .map(|c| Record::RelayerChainRegistration(make_relayer(c + 1)))
        .collect();
    let chunks: Vec<ChunkPlan> = Chunker::new(input.into_iter()).collect();
    assert_eq!(chunks.len(), 2);
    assert!(matches!(
        &chunks[0],
        ChunkPlan::BackfillRelayerRegistration(e) if e.len() == MAX_RELAYER_REGISTRATION_ENTRIES_PER_CHUNK
    ));
    assert!(matches!(
        &chunks[1],
        ChunkPlan::BackfillRelayerRegistration(e) if e.len() == 1
    ));
}

#[test]
fn relayer_registrations_split_on_non_ascending_chain() {
    // A duplicate chain breaks strict-ascending → the chunker closes the
    // chunk rather than packing an entry the program would reject.
    let input = vec![
        Record::RelayerChainRegistration(make_relayer(3)),
        Record::RelayerChainRegistration(make_relayer(3)), // not > 3
    ];
    let chunks: Vec<ChunkPlan> = Chunker::new(input.into_iter()).collect();
    assert_eq!(chunks.len(), 2);
    assert!(matches!(
        &chunks[0],
        ChunkPlan::BackfillRelayerRegistration(e) if e.len() == 1
    ));
    assert!(matches!(
        &chunks[1],
        ChunkPlan::BackfillRelayerRegistration(e) if e.len() == 1
    ));
}

// ----- transceiver hub -----

#[test]
fn transceiver_hubs_pack_up_to_max() {
    let input: Vec<Record> = (0..MAX_TRANSCEIVER_HUB_ENTRIES_PER_CHUNK as u8)
        .map(|i| Record::TransceiverHub(make_hub(2, i + 1)))
        .collect();
    let chunks: Vec<ChunkPlan> = Chunker::new(input.into_iter()).collect();
    assert_eq!(chunks.len(), 1);
    match &chunks[0] {
        ChunkPlan::BackfillTransceiverHub(e) => {
            assert_eq!(e.len(), MAX_TRANSCEIVER_HUB_ENTRIES_PER_CHUNK);
            for w in e.windows(2) {
                assert!((w[0].chain, w[0].address) < (w[1].chain, w[1].address));
            }
        }
        other => panic!("expected BackfillTransceiverHub, got {other:?}"),
    }
}

#[test]
fn transceiver_hubs_overflow_splits_at_max() {
    let count = MAX_TRANSCEIVER_HUB_ENTRIES_PER_CHUNK + 2;
    let input: Vec<Record> = (0..count as u8)
        .map(|i| Record::TransceiverHub(make_hub(2, i + 1)))
        .collect();
    let chunks: Vec<ChunkPlan> = Chunker::new(input.into_iter()).collect();
    assert_eq!(chunks.len(), 2);
    assert!(matches!(
        &chunks[0],
        ChunkPlan::BackfillTransceiverHub(e) if e.len() == MAX_TRANSCEIVER_HUB_ENTRIES_PER_CHUNK
    ));
    assert!(matches!(
        &chunks[1],
        ChunkPlan::BackfillTransceiverHub(e) if e.len() == 2
    ));
}

#[test]
fn transceiver_hubs_split_on_non_ascending_key() {
    // Equal (chain, address) breaks strict-ascending.
    let input = vec![
        Record::TransceiverHub(make_hub(2, 5)),
        Record::TransceiverHub(make_hub(2, 5)),
    ];
    let chunks: Vec<ChunkPlan> = Chunker::new(input.into_iter()).collect();
    assert_eq!(chunks.len(), 2);
}

// ----- transceiver peer -----

#[test]
fn transceiver_peers_pack_up_to_max() {
    // Vary dest_chain to keep the triple strictly ascending.
    let input: Vec<Record> = (0..MAX_TRANSCEIVER_PEER_ENTRIES_PER_CHUNK as u16)
        .map(|i| Record::TransceiverPeer(make_peer(2, 7, i + 1)))
        .collect();
    let chunks: Vec<ChunkPlan> = Chunker::new(input.into_iter()).collect();
    assert_eq!(chunks.len(), 1);
    match &chunks[0] {
        ChunkPlan::BackfillTransceiverPeer(e) => {
            assert_eq!(e.len(), MAX_TRANSCEIVER_PEER_ENTRIES_PER_CHUNK);
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
    let count = MAX_TRANSCEIVER_PEER_ENTRIES_PER_CHUNK + 1;
    let input: Vec<Record> = (0..count as u16)
        .map(|i| Record::TransceiverPeer(make_peer(2, 7, i + 1)))
        .collect();
    let chunks: Vec<ChunkPlan> = Chunker::new(input.into_iter()).collect();
    assert_eq!(chunks.len(), 2);
    assert!(matches!(
        &chunks[0],
        ChunkPlan::BackfillTransceiverPeer(e) if e.len() == MAX_TRANSCEIVER_PEER_ENTRIES_PER_CHUNK
    ));
    assert!(matches!(
        &chunks[1],
        ChunkPlan::BackfillTransceiverPeer(e) if e.len() == 1
    ));
}

#[test]
fn transceiver_peers_split_on_non_ascending_key() {
    let input = vec![
        Record::TransceiverPeer(make_peer(2, 7, 4)),
        Record::TransceiverPeer(make_peer(2, 7, 4)),
    ];
    let chunks: Vec<ChunkPlan> = Chunker::new(input.into_iter()).collect();
    assert_eq!(chunks.len(), 2);
}

#[test]
fn ntt_kind_transition_closes_chunk() {
    // relayer → hub → peer: three distinct chunks, no cross-kind packing.
    let input = vec![
        Record::RelayerChainRegistration(make_relayer(1)),
        Record::TransceiverHub(make_hub(2, 1)),
        Record::TransceiverPeer(make_peer(2, 1, 3)),
    ];
    let chunks: Vec<ChunkPlan> = Chunker::new(input.into_iter()).collect();
    assert_eq!(chunks.len(), 3);
    assert!(matches!(
        chunks[0],
        ChunkPlan::BackfillRelayerRegistration(_)
    ));
    assert!(matches!(chunks[1], ChunkPlan::BackfillTransceiverHub(_)));
    assert!(matches!(chunks[2], ChunkPlan::BackfillTransceiverPeer(_)));
}
