//! Integration tests for the catalogue → chunk planner.

use ga_backfill::catalogue::{
    AccountRecord, ModificationRecord, ModifyKind, Record, RegistrationRecord, TransferRecord,
};
use ga_backfill::chunker::{
    ChunkPlan, Chunker, MAX_BALANCE_ENTRIES_PER_CHUNK, MAX_NOREPLAY_ENTRIES_PER_CHUNK,
};

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
