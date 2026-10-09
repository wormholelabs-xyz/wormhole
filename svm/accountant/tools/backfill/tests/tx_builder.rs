//! Integration tests for the tx builder.

use ga_backfill::accountant::Accountant;
use ga_backfill::expected_accounts::derive_pda;
use ga_backfill::noreplay::{derive_noreplay_authority_pda, derive_noreplay_bucket};
use ga_backfill::snapshot::{
    BalanceRow, ChainRegistrationRow, TransceiverHubRow, TransceiverPeerRow, TransferRow,
};
use ga_backfill::tx_builder::{
    build_backfill_balance_ix, build_backfill_chain_registration_ix, build_backfill_noreplay_ix,
    build_backfill_transceiver_hub_ix, build_backfill_transceiver_peer_ix, BackfillTarget,
};
use global_accountant_definitions::{
    BalanceAccountKey, ChainRegistrationKey, RegisterChainKey, TransceiverHubKey,
    TransceiverPeerKey,
};
use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use solana_signer::Signer;

fn make_relayer(chain: u16, emitter_seed: u8) -> ChainRegistrationRow {
    let mut registered_emitter = [0u8; 32];
    registered_emitter[31] = emitter_seed;
    ChainRegistrationRow {
        chain,
        registered_emitter,
        sequence: 0x0102_0304_0506_0700 | u64::from(chain),
    }
}

fn make_hub(chain: u16, addr_seed: u8, hub_chain: u16, hub_seed: u8) -> TransceiverHubRow {
    let mut address = [0u8; 32];
    address[31] = addr_seed;
    let mut hub_address = [0u8; 32];
    hub_address[31] = hub_seed;
    TransceiverHubRow {
        chain,
        address,
        hub_chain,
        hub_address,
    }
}

fn make_peer(chain: u16, addr_seed: u8, dest_chain: u16, peer_seed: u8) -> TransceiverPeerRow {
    let mut address = [0u8; 32];
    address[31] = addr_seed;
    let mut peer_address = [0u8; 32];
    peer_address[31] = peer_seed;
    TransceiverPeerRow {
        chain,
        address,
        dest_chain,
        peer_address,
    }
}

/// The signer must match the accountant's compile-time backfill authority;
/// `BackfillTarget::new` asserts this. Test authorities: `[1u8; 32]` (WTT),
/// `[2u8; 32]` (NTT).
fn payer(accountant: Accountant) -> Pubkey {
    let seed = match accountant {
        Accountant::Wtt => 1u8,
        Accountant::Ntt => 2u8,
    };
    Keypair::new_from_array([seed; 32]).pubkey()
}

fn target() -> BackfillTarget {
    BackfillTarget::new(
        Accountant::Wtt,
        Accountant::Wtt.program_id(),
        payer(Accountant::Wtt),
    )
}

fn ntt_ctx() -> BackfillTarget {
    BackfillTarget::new(
        Accountant::Ntt,
        Accountant::Ntt.program_id(),
        payer(Accountant::Ntt),
    )
}

fn make_transfer(chain: u16, emitter_seed: u8, sequence: u64) -> TransferRow {
    let mut emitter = [0u8; 32];
    emitter[31] = emitter_seed;
    let mut digest = [0u8; 32];
    digest[31] = (sequence & 0xff) as u8;
    TransferRow {
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

fn make_account(chain: u16, token_chain: u16, addr_seed: u8, balance_low: u8) -> BalanceRow {
    let mut token_address = [0u8; 32];
    token_address[31] = addr_seed;
    let mut balance = [0u8; 32];
    balance[31] = balance_low;
    BalanceRow {
        chain,
        token_chain,
        token_address,
        balance,
    }
}

// ============================================================================
// PDA derivation tests
// ============================================================================

#[test]
fn pda_derivations_are_deterministic() {
    let c = target();
    let np_auth = derive_noreplay_authority_pda(&c.program_id);
    let np_auth2 = derive_noreplay_authority_pda(&c.program_id);
    assert_eq!(np_auth, np_auth2);
}

#[test]
fn noreplay_bucket_changes_at_1024_boundary() {
    let np_auth = derive_noreplay_authority_pda(&target().program_id);
    let emitter = [0xAAu8; 32];
    let noreplay = target().noreplay_program;
    let bucket_0 = derive_noreplay_bucket(&noreplay, &np_auth, 1, &emitter, 0);
    let bucket_0b = derive_noreplay_bucket(&noreplay, &np_auth, 1, &emitter, 1023);
    let bucket_1 = derive_noreplay_bucket(&noreplay, &np_auth, 1, &emitter, 1024);
    assert_eq!(
        bucket_0, bucket_0b,
        "sequences 0..1023 must hit the same bucket"
    );
    assert_ne!(bucket_0, bucket_1, "sequence 1024 must spill to bucket 1");
}

#[test]
fn balance_pda_depends_on_full_triple() {
    let c = target();
    let pda_a = derive_pda(&c.program_id, &BalanceAccountKey::new(1, 2, [0u8; 32]));
    let pda_b = derive_pda(&c.program_id, &BalanceAccountKey::new(1, 2, [1u8; 32])); // diff token_address
    let pda_c = derive_pda(&c.program_id, &BalanceAccountKey::new(1, 3, [0u8; 32])); // diff token_chain
    let pda_d = derive_pda(&c.program_id, &BalanceAccountKey::new(2, 2, [0u8; 32])); // diff chain
    assert_ne!(pda_a, pda_b);
    assert_ne!(pda_a, pda_c);
    assert_ne!(pda_a, pda_d);
}

#[test]
#[should_panic(expected = "is not the wtt backfill authority")]
fn ctx_new_rejects_wrong_payer() {
    // Any pubkey other than the backfill authority must trip the assertion;
    // otherwise the orchestrator would send txs that the program rejects
    // with `UnauthorizedCaller`.
    BackfillTarget::new(
        Accountant::Wtt,
        Accountant::Wtt.program_id(),
        Pubkey::new_from_array([0xDEu8; 32]),
    );
}

// ============================================================================
// BackfillNoReplay tx builder
// ============================================================================

#[test]
fn backfill_noreplay_single_entry_wire() {
    let c = target();
    let ix = build_backfill_noreplay_ix(&c, &[make_transfer(2, 0xAA, 42)]);

    // Wire: [disc][group_count][chain emitter entry_count [seq digest]]
    assert_eq!(ix.data[0], c.accountant.discriminators().noreplay);
    assert_eq!(ix.data[1], 1, "one group");
    // group 0: chain (BE 2)
    assert_eq!(&ix.data[2..4], &[0x00, 0x02]);
    // group 0: emitter (32 B; last byte 0xAA)
    assert_eq!(ix.data[35], 0xAA);
    // group 0: entry_count
    assert_eq!(ix.data[36], 1);
    // entry 0: sequence (BE 8)
    assert_eq!(&ix.data[37..45], &42u64.to_be_bytes());
    // entry 0: digest (32 B)
    assert_eq!(ix.data[76], 42);
    assert_eq!(ix.data.len(), 77);
}

#[test]
fn backfill_noreplay_groups_by_emitter() {
    // 2 from emitter AA, 3 from emitter BB. Builder should emit 2 groups.
    let c = target();
    let transfers = vec![
        make_transfer(1, 0xAA, 5),
        make_transfer(1, 0xAA, 6),
        make_transfer(1, 0xBB, 10),
        make_transfer(1, 0xBB, 11),
        make_transfer(1, 0xBB, 12),
    ];
    let ix = build_backfill_noreplay_ix(&c, &transfers);
    assert_eq!(ix.data[1], 2, "two groups");

    // Group 1: chain=1, emitter[31]=AA, count=2 → 35 bytes header + 2 entries × 40 = 115 bytes
    assert_eq!(&ix.data[2..4], &[0x00, 0x01]);
    assert_eq!(ix.data[35], 0xAA);
    assert_eq!(ix.data[36], 2);

    // Group 2 starts at offset 2 + 35 + 2*40 = 117
    let g2_start = 2 + 35 + 2 * 40;
    assert_eq!(&ix.data[g2_start..g2_start + 2], &[0x00, 0x01]);
    assert_eq!(ix.data[g2_start + 33], 0xBB);
    assert_eq!(ix.data[g2_start + 34], 3);
}

#[test]
fn backfill_noreplay_account_list_shape() {
    // 3 transfers in 1 bucket → fixed 4 + 1 bucket pda = 5 accounts
    let c = target();
    let transfers = vec![
        make_transfer(1, 0xAA, 5),
        make_transfer(1, 0xAA, 6),
        make_transfer(1, 0xAA, 7),
    ];
    let ix = build_backfill_noreplay_ix(&c, &transfers);
    assert_eq!(ix.accounts.len(), 5);

    // [0] payer — signer + writable
    assert_eq!(ix.accounts[0].pubkey, c.payer);
    assert!(ix.accounts[0].is_signer);
    assert!(ix.accounts[0].is_writable);
    // [1] noreplay_program — readonly
    assert_eq!(ix.accounts[1].pubkey, c.noreplay_program);
    assert!(!ix.accounts[1].is_writable);
    // [2] noreplay_authority — readonly
    assert_eq!(
        ix.accounts[2].pubkey,
        derive_noreplay_authority_pda(&c.program_id)
    );
    assert!(!ix.accounts[2].is_writable);
    // [3] system_program
    assert_eq!(ix.accounts[3].pubkey, c.system_program);
    // [4] bucket pda — writable
    assert!(ix.accounts[4].is_writable);
}

#[test]
fn backfill_noreplay_buckets_span_1024_boundary() {
    // Transfers spanning sequences 1022..1025 produce 2 unique bucket PDAs.
    let c = target();
    let transfers = vec![
        make_transfer(1, 0xAA, 1022),
        make_transfer(1, 0xAA, 1023),
        make_transfer(1, 0xAA, 1024),
        make_transfer(1, 0xAA, 1025),
    ];
    let ix = build_backfill_noreplay_ix(&c, &transfers);
    // 4 fixed + 2 buckets = 6 accounts
    assert_eq!(ix.accounts.len(), 6);
}

// ============================================================================
// BackfillBalance tx builder
// ============================================================================

#[test]
fn backfill_balance_single_entry_wire() {
    let c = target();
    let ix = build_backfill_balance_ix(&c, &[make_account(1, 2, 0xCC, 0x42)]);

    // Wire: [disc][count] [chain token_chain token_address balance]
    assert_eq!(ix.data[0], c.accountant.discriminators().balance);
    assert_eq!(ix.data[1], 1);
    assert_eq!(&ix.data[2..4], &[0x00, 0x01]); // chain
    assert_eq!(&ix.data[4..6], &[0x00, 0x02]); // token_chain
    assert_eq!(ix.data[37], 0xCC); // token_address last byte
    assert_eq!(ix.data[69], 0x42); // balance last byte
    assert_eq!(ix.data.len(), 2 + 68);
}

#[test]
fn backfill_balance_bulk_entries_in_order() {
    let c = target();
    let accounts = vec![
        make_account(1, 2, 0xAA, 1),
        make_account(1, 2, 0xBB, 2),
        make_account(1, 2, 0xCC, 3),
    ];
    let ix = build_backfill_balance_ix(&c, &accounts);
    assert_eq!(ix.data[1], 3);
    // 3 entries × 68 bytes + 2 byte header = 206 bytes
    assert_eq!(ix.data.len(), 2 + 3 * 68);
    // entries preserve input order
    assert_eq!(ix.data[2 + 35], 0xAA); // entry 0 token_address last byte
    assert_eq!(ix.data[2 + 68 + 35], 0xBB);
    assert_eq!(ix.data[2 + 2 * 68 + 35], 0xCC);
}

#[test]
fn backfill_balance_account_list_shape() {
    let c = target();
    let accounts = vec![make_account(1, 2, 0xAA, 1), make_account(1, 2, 0xBB, 2)];
    let ix = build_backfill_balance_ix(&c, &accounts);

    // Fixed 2 + N PDAs = 4
    assert_eq!(ix.accounts.len(), 4);
    assert_eq!(ix.accounts[0].pubkey, c.payer);
    assert!(ix.accounts[0].is_signer);
    assert_eq!(ix.accounts[1].pubkey, c.system_program);
    // PDAs: writable, in entry order
    assert!(ix.accounts[2].is_writable);
    assert!(ix.accounts[3].is_writable);
    assert_eq!(
        ix.accounts[2].pubkey,
        derive_pda(
            &c.program_id,
            &BalanceAccountKey::new(1, 2, accounts[0].token_address)
        )
    );
    assert_eq!(
        ix.accounts[3].pubkey,
        derive_pda(
            &c.program_id,
            &BalanceAccountKey::new(1, 2, accounts[1].token_address)
        )
    );
}

// ============================================================================
// Source-of-truth pinning
// ============================================================================

#[test]
fn discriminators_match_program_enum() {
    // Drift here would route txs to the wrong handler.
    use global_accountant_backfill::Instruction as PI;
    let d = Accountant::Wtt.discriminators();
    assert_eq!(d.noreplay, PI::BackfillNoReplay as u8);
    assert_eq!(d.balance, PI::BackfillBalance as u8);
    assert_eq!(d.modify_balance, PI::BackfillModifyBalance as u8);
    assert_eq!(d.chain_registration, PI::BackfillChainRegistration as u8);
    assert_eq!(d.transceiver_hub, None);
    assert_eq!(d.transceiver_peer, None);
}

#[test]
fn ntt_discriminators_match_program_enum() {
    use ntt_global_accountant_backfill::Instruction as NI;
    let d = Accountant::Ntt.discriminators();
    assert_eq!(d.noreplay, NI::BackfillNoReplay as u8);
    assert_eq!(d.balance, NI::BackfillBalance as u8);
    assert_eq!(d.modify_balance, NI::BackfillModifyBalance as u8);
    assert_eq!(
        d.chain_registration,
        NI::BackfillRelayerChainRegistration as u8
    );
    assert_eq!(d.transceiver_hub, Some(NI::BackfillTransceiverHub as u8));
    assert_eq!(d.transceiver_peer, Some(NI::BackfillTransceiverPeer as u8));
    // Literal wire values per the program's `#[instruction(discriminator = N)]`.
    assert_eq!(d.chain_registration, 3);
    assert_eq!(d.transceiver_hub, Some(4));
    assert_eq!(d.transceiver_peer, Some(5));
}

// ============================================================================
// BackfillRelayerChainRegistration tx builder
// ============================================================================

#[test]
fn relayer_registration_single_entry_wire() {
    let c = ntt_ctx();
    let entry = make_relayer(2, 0x85);
    let ix = build_backfill_chain_registration_ix(&c, std::slice::from_ref(&entry));
    // Wire: [disc][count] [chain(BE 2) sequence(BE 8) emitter(32)]
    assert_eq!(ix.data[0], c.accountant.discriminators().chain_registration);
    assert_eq!(ix.data[1], 1);
    assert_eq!(&ix.data[2..4], &[0x00, 0x02]); // chain BE
    assert_eq!(&ix.data[4..12], &entry.sequence.to_be_bytes()); // sequence BE
    assert_eq!(ix.data[43], 0x85); // emitter last byte
    assert_eq!(ix.data.len(), 2 + 42);
}

#[test]
fn relayer_registration_bulk_wire_and_pdas() {
    let c = ntt_ctx();
    let entries = vec![make_relayer(2, 0xAA), make_relayer(5, 0xBB)];
    let ix = build_backfill_chain_registration_ix(&c, &entries);
    assert_eq!(ix.data[1], 2);
    assert_eq!(ix.data.len(), 2 + 2 * 42);
    // entry 1 starts at 2 + 42 = 44
    assert_eq!(&ix.data[44..46], &[0x00, 0x05]);
    assert_eq!(ix.data[44 + 41], 0xBB);

    // payer + system + per entry (ChainRegistration, RegisterChain), writable, in order
    assert_eq!(ix.accounts.len(), 6);
    assert_eq!(ix.accounts[0].pubkey, c.payer);
    assert!(ix.accounts[0].is_signer && ix.accounts[0].is_writable);
    assert_eq!(ix.accounts[1].pubkey, c.system_program);
    assert!(!ix.accounts[1].is_writable);
    assert!(ix.accounts[2..].iter().all(|m| m.is_writable));
    for (i, e) in entries.iter().enumerate() {
        assert_eq!(
            ix.accounts[2 + 2 * i].pubkey,
            derive_pda(&c.program_id, &ChainRegistrationKey::new(e.chain))
        );
        assert_eq!(
            ix.accounts[3 + 2 * i].pubkey,
            derive_pda(&c.program_id, &RegisterChainKey::new(e.sequence))
        );
    }
}

#[test]
fn relayer_registration_pda_depends_on_chain() {
    let c = ntt_ctx();
    assert_ne!(
        derive_pda(&c.program_id, &ChainRegistrationKey::new(2)),
        derive_pda(&c.program_id, &ChainRegistrationKey::new(3))
    );
}

// ============================================================================
// BackfillTransceiverHub tx builder
// ============================================================================

#[test]
fn transceiver_hub_single_entry_wire() {
    let c = ntt_ctx();
    let ix = build_backfill_transceiver_hub_ix(&c, &[make_hub(2, 0x22, 1, 0x33)]).expect("ntt");
    // Wire: [disc][count] [chain(2) address(32) hub_chain(2) hub_address(32)]
    assert_eq!(
        Some(ix.data[0]),
        c.accountant.discriminators().transceiver_hub
    );
    assert_eq!(ix.data[1], 1);
    // Entry at offset 2: chain@2..4, address@4..36, hub_chain@36..38, hub_address@38..70.
    assert_eq!(&ix.data[2..4], &[0x00, 0x02]); // chain BE
    assert_eq!(ix.data[35], 0x22); // address last byte @ 4+32-1 = 35
    assert_eq!(&ix.data[36..38], &[0x00, 0x01]); // hub_chain BE @ 36
    assert_eq!(ix.data[69], 0x33); // hub_address last byte @ 38+32-1 = 69
    assert_eq!(ix.data.len(), 2 + 68);
}

#[test]
fn transceiver_hub_bulk_wire_and_pdas() {
    let c = ntt_ctx();
    let entries = vec![make_hub(2, 0x22, 1, 0x33), make_hub(2, 0x44, 1, 0x55)];
    let ix = build_backfill_transceiver_hub_ix(&c, &entries).expect("ntt");
    assert_eq!(ix.data[1], 2);
    assert_eq!(ix.data.len(), 2 + 2 * 68);
    // entry 1 starts at 2+68=70; address occupies 70+2..70+34, last byte @ 70+33 = 103
    assert_eq!(ix.data[2 + 68 + 33], 0x44);

    assert_eq!(ix.accounts.len(), 4);
    assert_eq!(ix.accounts[0].pubkey, c.payer);
    assert_eq!(ix.accounts[1].pubkey, c.system_program);
    assert!(ix.accounts[2].is_writable);
    assert_eq!(
        ix.accounts[2].pubkey,
        derive_pda(
            &c.program_id,
            &TransceiverHubKey::new(2, entries[0].address)
        )
    );
    assert_eq!(
        ix.accounts[3].pubkey,
        derive_pda(
            &c.program_id,
            &TransceiverHubKey::new(2, entries[1].address)
        )
    );
}

#[test]
fn transceiver_hub_pda_depends_on_chain_and_address() {
    let c = target();
    let a = derive_pda(&c.program_id, &TransceiverHubKey::new(2, [1u8; 32]));
    let b = derive_pda(&c.program_id, &TransceiverHubKey::new(2, [2u8; 32])); // diff address
    let d = derive_pda(&c.program_id, &TransceiverHubKey::new(3, [1u8; 32])); // diff chain
    assert_ne!(a, b);
    assert_ne!(a, d);
}

// ============================================================================
// BackfillTransceiverPeer tx builder
// ============================================================================

#[test]
fn transceiver_peer_single_entry_wire() {
    let c = ntt_ctx();
    let ix = build_backfill_transceiver_peer_ix(&c, &[make_peer(2, 0x66, 4, 0x77)]).expect("ntt");
    // Wire: [disc][count] [chain(2) address(32) dest_chain(2) peer_address(32)]
    assert_eq!(
        Some(ix.data[0]),
        c.accountant.discriminators().transceiver_peer
    );
    assert_eq!(ix.data[1], 1);
    // Entry at offset 2: chain@2..4, address@4..36, dest_chain@36..38, peer_address@38..70.
    assert_eq!(&ix.data[2..4], &[0x00, 0x02]); // chain BE
    assert_eq!(ix.data[35], 0x66); // address last byte @ 35
    assert_eq!(&ix.data[36..38], &[0x00, 0x04]); // dest_chain BE @ 36
    assert_eq!(ix.data[69], 0x77); // peer_address last byte @ 69
    assert_eq!(ix.data.len(), 2 + 68);
}

#[test]
fn transceiver_peer_bulk_wire_and_pdas() {
    let c = ntt_ctx();
    let entries = vec![make_peer(2, 0x66, 4, 0x77), make_peer(2, 0x66, 5, 0x88)];
    let ix = build_backfill_transceiver_peer_ix(&c, &entries).expect("ntt");
    assert_eq!(ix.data[1], 2);
    assert_eq!(ix.data.len(), 2 + 2 * 68);
    // entry 1 dest_chain at 2 + 68 + 34 = 104
    assert_eq!(&ix.data[2 + 68 + 34..2 + 68 + 36], &[0x00, 0x05]);

    assert_eq!(ix.accounts.len(), 4);
    assert_eq!(ix.accounts[0].pubkey, c.payer);
    assert_eq!(ix.accounts[1].pubkey, c.system_program);
    assert!(ix.accounts[2].is_writable);
    assert_eq!(
        ix.accounts[2].pubkey,
        derive_pda(
            &c.program_id,
            &TransceiverPeerKey::new(2, entries[0].address, 4)
        )
    );
    assert_eq!(
        ix.accounts[3].pubkey,
        derive_pda(
            &c.program_id,
            &TransceiverPeerKey::new(2, entries[1].address, 5)
        )
    );
}

#[test]
fn transceiver_peer_pda_depends_on_full_triple() {
    let c = target();
    let a = derive_pda(&c.program_id, &TransceiverPeerKey::new(2, [1u8; 32], 4));
    let b = derive_pda(&c.program_id, &TransceiverPeerKey::new(2, [1u8; 32], 5)); // diff dest_chain
    let d = derive_pda(&c.program_id, &TransceiverPeerKey::new(2, [2u8; 32], 4)); // diff address
    let e = derive_pda(&c.program_id, &TransceiverPeerKey::new(3, [1u8; 32], 4)); // diff chain
    assert_ne!(a, b);
    assert_ne!(a, d);
    assert_ne!(a, e);
}

#[test]
fn backfill_authority_pubkey_matches_program_const() {
    // The test keypairs must match the authorities compiled into the program
    // crates; a bumped const without a bumped keypair fails here first.
    use global_accountant_backfill::BACKFILL_AUTHORITY;
    use ntt_global_accountant_backfill::NTT_BACKFILL_AUTHORITY;
    assert_eq!(Accountant::Wtt.authority().to_bytes(), BACKFILL_AUTHORITY);
    assert_eq!(payer(Accountant::Wtt).to_bytes(), BACKFILL_AUTHORITY);
    assert_eq!(
        Accountant::Ntt.authority().to_bytes(),
        NTT_BACKFILL_AUTHORITY
    );
    assert_eq!(payer(Accountant::Ntt).to_bytes(), NTT_BACKFILL_AUTHORITY);
}
