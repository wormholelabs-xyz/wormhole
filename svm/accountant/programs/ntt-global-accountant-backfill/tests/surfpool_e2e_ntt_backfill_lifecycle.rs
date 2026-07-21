//! Surfpool E2E — NTT backfill program lifecycle against a real subprocess
//! validator, with the production `solana_noreplay.so` co-deployed.
//!
//! Phases the test exercises in order:
//!
//! 1. `BackfillNoReplay` for three entries spanning two buckets — assert the
//!    NoReplay bitmap bits flip and the canonical `ACCDGST\0` commit-log
//!    entries appear via `meta.logMessages` over real RPC.
//! 2. `BackfillBalance` for two accounts — assert both `BalanceAccountLayout`
//!    PDAs are written at canonical seeds via `getAccountInfo`.
//! 3. `BackfillRelayerRegistration` for two chains — assert both
//!    `RelayerChainRegistrationLayout` PDAs are written.
//! 4. `BackfillTransceiverHub` for one entry — assert the `TransceiverHubLayout`
//!    PDA is written.
//! 5. `BackfillTransceiverPeer` for one entry — assert the `TransceiverPeerLayout`
//!    PDA is written.
//! 6. `BackfillRelayerRegistration` signed by a non-authority keypair — assert
//!    the tx fails with `UnauthorizedCaller` (Custom(3)).
//! 7. `BackfillRelayerRegistration` signed by the *WTT* backfill program's
//!    real operator authority — assert it still fails with
//!    `UnauthorizedCaller` against the live, deployed NTT `.so`.

#![allow(clippy::too_many_arguments)]

use std::{str::FromStr, time::Duration};

use solana_client::{client_error::ClientError, rpc_client::RpcClient};
use solana_instruction::{AccountMeta, Instruction};
use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use solana_rent::Rent;
use solana_signer::Signer;
use solana_transaction::Transaction;

use global_accountant_definitions::{
    BalanceAccountLayout, RelayerChainRegistrationLayout, TransceiverHubLayout,
    TransceiverPeerLayout, Uint256, ACCOUNT_SEED_PREFIX, NOREPLAY_AUTHORITY_SEED_PREFIX,
    NOREPLAY_BITMAP_OFFSET, NOREPLAY_BITS_PER_BUCKET, NOREPLAY_PROGRAM_ID,
    RELAYER_CHAIN_REGISTRATION_SEED_PREFIX, TRANSCEIVER_HUB_SEED_PREFIX,
    TRANSCEIVER_PEER_SEED_PREFIX,
};
use ntt_global_accountant_backfill::Instruction as IxDiscriminator;

mod common;
use common::surfpool::{
    await_confirmed, deploy_program, fetch_accdgst_logs, so_path, start_surfpool, SurfpoolOptions,
};

const BACKFILL_PROGRAM_NAME: &str = "ntt_global_accountant_backfill";

fn noreplay_so_path() -> std::path::PathBuf {
    // Vendored under this program's own fixtures so the suite is self-contained.
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/solana_noreplay.so")
}

// ============================================================================
// PDA derivations
// ============================================================================

fn derive_noreplay_authority_pda(program_id: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[NOREPLAY_AUTHORITY_SEED_PREFIX], program_id)
}

fn derive_noreplay_bucket(
    authority: &Pubkey,
    chain: u16,
    emitter: &[u8; 32],
    sequence: u64,
) -> Pubkey {
    let mut namespace = [0u8; 34];
    namespace[..2].copy_from_slice(&chain.to_be_bytes());
    namespace[2..].copy_from_slice(emitter);
    let bucket_index = (sequence / NOREPLAY_BITS_PER_BUCKET).to_le_bytes();
    let (pda, _) = Pubkey::find_program_address(
        &[
            authority.as_ref(),
            &namespace[..32],
            &namespace[32..],
            &bucket_index,
        ],
        &Pubkey::new_from_array(NOREPLAY_PROGRAM_ID),
    );
    pda
}

fn derive_balance_pda(
    program_id: &Pubkey,
    chain: u16,
    token_chain: u16,
    token_address: &[u8; 32],
) -> Pubkey {
    let chain_be = chain.to_be_bytes();
    let token_chain_be = token_chain.to_be_bytes();
    let (pda, _) = Pubkey::find_program_address(
        &[
            ACCOUNT_SEED_PREFIX,
            &chain_be,
            &token_chain_be,
            token_address,
        ],
        program_id,
    );
    pda
}

fn derive_relayer_pda(program_id: &Pubkey, chain: u16) -> Pubkey {
    Pubkey::find_program_address(
        &[RELAYER_CHAIN_REGISTRATION_SEED_PREFIX, &chain.to_be_bytes()],
        program_id,
    )
    .0
}

fn derive_hub_pda(program_id: &Pubkey, chain: u16, address: &[u8; 32]) -> Pubkey {
    Pubkey::find_program_address(
        &[TRANSCEIVER_HUB_SEED_PREFIX, &chain.to_be_bytes(), address],
        program_id,
    )
    .0
}

fn derive_peer_pda(program_id: &Pubkey, chain: u16, address: &[u8; 32], dest_chain: u16) -> Pubkey {
    Pubkey::find_program_address(
        &[
            TRANSCEIVER_PEER_SEED_PREFIX,
            &chain.to_be_bytes(),
            address,
            &dest_chain.to_be_bytes(),
        ],
        program_id,
    )
    .0
}

// ============================================================================
// Ix data builders
// ============================================================================

#[derive(Clone, Copy)]
struct NoReplayEntry {
    chain: u16,
    emitter: [u8; 32],
    sequence: u64,
    digest: [u8; 32],
}

/// Compact emitter-grouped wire format:
/// `[disc][group_count] [chain emitter entry_count [seq digest]...]...`
/// Caller must pre-sort entries by `(chain, emitter, sequence)`.
fn build_backfill_noreplay_data(entries: &[NoReplayEntry]) -> Vec<u8> {
    let mut groups: Vec<Vec<NoReplayEntry>> = Vec::new();
    let mut current: Vec<NoReplayEntry> = Vec::new();
    let mut current_key: Option<(u16, [u8; 32])> = None;
    for e in entries {
        let key = (e.chain, e.emitter);
        if current_key != Some(key) {
            if !current.is_empty() {
                groups.push(std::mem::take(&mut current));
            }
            current_key = Some(key);
        }
        current.push(*e);
    }
    if !current.is_empty() {
        groups.push(current);
    }
    let mut data = Vec::new();
    data.push(IxDiscriminator::BackfillNoReplay as u8);
    data.push(groups.len() as u8);
    for group in &groups {
        let first = &group[0];
        data.extend_from_slice(&first.chain.to_be_bytes());
        data.extend_from_slice(&first.emitter);
        data.push(group.len() as u8);
        for e in group {
            data.extend_from_slice(&e.sequence.to_be_bytes());
            data.extend_from_slice(&e.digest);
        }
    }
    data
}

#[derive(Clone, Copy)]
struct BalanceEntry {
    chain: u16,
    token_chain: u16,
    token_address: [u8; 32],
    balance: [u8; 32],
}

fn build_backfill_balance_data(entries: &[BalanceEntry]) -> Vec<u8> {
    let mut data = Vec::with_capacity(2 + entries.len() * 68);
    data.push(IxDiscriminator::BackfillBalance as u8);
    data.push(entries.len() as u8);
    for e in entries {
        data.extend_from_slice(&e.chain.to_be_bytes());
        data.extend_from_slice(&e.token_chain.to_be_bytes());
        data.extend_from_slice(&e.token_address);
        data.extend_from_slice(&e.balance);
    }
    data
}

fn build_backfill_relayer_registration_data(entries: &[(u16, [u8; 32])]) -> Vec<u8> {
    let mut data = vec![
        IxDiscriminator::BackfillRelayerRegistration as u8,
        entries.len() as u8,
    ];
    for (chain, emitter) in entries {
        data.extend_from_slice(&chain.to_be_bytes());
        data.extend_from_slice(emitter);
    }
    data
}

fn build_backfill_transceiver_hub_data(entries: &[(u16, [u8; 32], u16, [u8; 32])]) -> Vec<u8> {
    let mut data = vec![
        IxDiscriminator::BackfillTransceiverHub as u8,
        entries.len() as u8,
    ];
    for (chain, address, hub_chain, hub_address) in entries {
        data.extend_from_slice(&chain.to_be_bytes());
        data.extend_from_slice(address);
        data.extend_from_slice(&hub_chain.to_be_bytes());
        data.extend_from_slice(hub_address);
    }
    data
}

fn build_backfill_transceiver_peer_data(entries: &[(u16, [u8; 32], u16, [u8; 32])]) -> Vec<u8> {
    let mut data = vec![
        IxDiscriminator::BackfillTransceiverPeer as u8,
        entries.len() as u8,
    ];
    for (chain, address, dest_chain, peer_address) in entries {
        data.extend_from_slice(&chain.to_be_bytes());
        data.extend_from_slice(address);
        data.extend_from_slice(&dest_chain.to_be_bytes());
        data.extend_from_slice(peer_address);
    }
    data
}

fn system_program_id() -> Pubkey {
    Pubkey::from_str("11111111111111111111111111111111").unwrap()
}

fn send_ix(rpc: &RpcClient, payer: &Keypair, ix: Instruction) -> Result<String, ClientError> {
    let blockhash = rpc.get_latest_blockhash()?;
    let tx = Transaction::new_signed_with_payer(&[ix], Some(&payer.pubkey()), &[payer], blockhash);
    rpc.send_and_confirm_transaction(&tx).map(|s| s.to_string())
}

// ============================================================================
// The test
// ============================================================================

#[test]
#[ignore = "spawns surfpool subprocess; run via `cargo test --test surfpool_e2e_ntt_backfill_lifecycle -- --ignored`"]
fn surfpool_ntt_backfill_lifecycle() {
    // ---------- Load .so artefacts ----------
    let backfill_so = so_path(BACKFILL_PROGRAM_NAME);
    let backfill_bytes = std::fs::read(&backfill_so).unwrap_or_else(|e| {
        panic!(
            "could not read {}: {e}. Run `just build` first.",
            backfill_so.display()
        )
    });
    let noreplay_bytes = std::fs::read(noreplay_so_path()).expect("solana_noreplay.so fixture");
    eprintln!(
        "[ntt-backfill-e2e] backfill={} bytes, noreplay={} bytes",
        backfill_bytes.len(),
        noreplay_bytes.len()
    );

    // ---------- Boot surfpool offline ----------
    let guard = start_surfpool(SurfpoolOptions::offline("ntt-ga-backfill-e2e"));
    let rpc_url = guard.rpc_url();
    let rpc = guard.rpc_client();

    // ---------- Fresh program keypair + authority/payer airdrop ----------
    let program_kp = Keypair::new();
    let program_id = program_kp.pubkey();
    // Must equal the NTT program's BACKFILL_AUTHORITY (seed [2u8; 32]),
    // distinct from WTT's own e2e authority (seed [1u8; 32]).
    let authority = Keypair::new_from_array([2u8; 32]);
    let stranger = Keypair::new();
    // WTT backfill program's real operator authority (seed [1u8; 32]) — used
    // in Phase 7 to prove it's rejected by the NTT program.
    let wtt_authority = Keypair::new_from_array([1u8; 32]);
    eprintln!(
        "[ntt-backfill-e2e] program_id={program_id} authority={} stranger={} wtt_authority={}",
        authority.pubkey(),
        stranger.pubkey(),
        wtt_authority.pubkey()
    );

    let airdrop = rpc
        .request_airdrop(&authority.pubkey(), 20_000_000_000)
        .expect("airdrop authority");
    await_confirmed("airdrop", Duration::from_secs(10), || {
        rpc.confirm_transaction(&airdrop)
    });
    let stranger_drop = rpc
        .request_airdrop(&stranger.pubkey(), 2_000_000_000)
        .expect("airdrop stranger");
    await_confirmed("airdrop-stranger", Duration::from_secs(10), || {
        rpc.confirm_transaction(&stranger_drop)
    });
    let wtt_authority_drop = rpc
        .request_airdrop(&wtt_authority.pubkey(), 2_000_000_000)
        .expect("airdrop wtt_authority");
    await_confirmed("airdrop-wtt-authority", Duration::from_secs(10), || {
        rpc.confirm_transaction(&wtt_authority_drop)
    });

    deploy_program(&rpc_url, &program_id, &backfill_bytes);
    deploy_program(
        &rpc_url,
        &Pubkey::new_from_array(NOREPLAY_PROGRAM_ID),
        &noreplay_bytes,
    );

    // ---------- Derive PDAs ----------
    let (noreplay_auth_pda, _) = derive_noreplay_authority_pda(&program_id);

    // ---------- Phase 1: BackfillNoReplay (3 entries, 2 buckets) ----------
    let emitter = [0x42u8; 32];
    let entries = [
        NoReplayEntry {
            chain: 2,
            emitter,
            sequence: 10,
            digest: [0xa1u8; 32],
        },
        NoReplayEntry {
            chain: 2,
            emitter,
            sequence: 500,
            digest: [0xa2u8; 32],
        },
        NoReplayEntry {
            chain: 2,
            emitter,
            sequence: 1500,
            digest: [0xa3u8; 32],
        },
    ];
    let bucket_0 = derive_noreplay_bucket(&noreplay_auth_pda, 2, &emitter, 0);
    let bucket_1 = derive_noreplay_bucket(&noreplay_auth_pda, 2, &emitter, 1500);

    let metas_no_replay = vec![
        AccountMeta::new(authority.pubkey(), true),
        AccountMeta::new_readonly(Pubkey::new_from_array(NOREPLAY_PROGRAM_ID), false),
        AccountMeta::new_readonly(noreplay_auth_pda, false),
        AccountMeta::new_readonly(system_program_id(), false),
        AccountMeta::new(bucket_0, false),
        AccountMeta::new(bucket_1, false),
    ];
    let ix = Instruction {
        program_id,
        accounts: metas_no_replay,
        data: build_backfill_noreplay_data(&entries),
    };
    let sig = send_ix(&rpc, &authority, ix).expect("BackfillNoReplay tx");
    eprintln!("[ntt-backfill-e2e] BackfillNoReplay tx={sig}");

    let logs = fetch_accdgst_logs(&rpc_url, &sig);
    assert_eq!(logs.len(), 3, "expected 3 commit-log entries");
    for (i, e) in entries.iter().enumerate() {
        let (chain, em, seq, dig, gsi) = &logs[i];
        assert_eq!(*chain, e.chain);
        assert_eq!(em, &e.emitter);
        assert_eq!(*seq, e.sequence);
        assert_eq!(dig, &e.digest);
        assert_eq!(*gsi, 0, "guardian_set_index sentinel");
    }

    let b0 = rpc.get_account(&bucket_0).expect("bucket_0 account");
    let b1 = rpc.get_account(&bucket_1).expect("bucket_1 account");
    for seq in [10u64, 500] {
        let bit = (seq % NOREPLAY_BITS_PER_BUCKET) as usize;
        let byte = b0.data[NOREPLAY_BITMAP_OFFSET + bit / 8];
        assert_eq!(byte & (1 << (bit % 8)), 1 << (bit % 8));
    }
    let bit = (1500u64 % NOREPLAY_BITS_PER_BUCKET) as usize;
    let byte = b1.data[NOREPLAY_BITMAP_OFFSET + bit / 8];
    assert_eq!(byte & (1 << (bit % 8)), 1 << (bit % 8));

    // ---------- Phase 2: BackfillBalance (2 accounts) ----------
    let balances = [
        BalanceEntry {
            chain: 2,
            token_chain: 2,
            token_address: [0x11u8; 32],
            balance: {
                let mut b = [0u8; 32];
                b[24..32].copy_from_slice(&1_000_000u64.to_be_bytes());
                b
            },
        },
        BalanceEntry {
            chain: 4,
            token_chain: 4,
            token_address: [0x22u8; 32],
            balance: {
                let mut b = [0u8; 32];
                b[24..32].copy_from_slice(&2_500_000u64.to_be_bytes());
                b
            },
        },
    ];
    let bal_pda_0 = derive_balance_pda(&program_id, 2, 2, &[0x11u8; 32]);
    let bal_pda_1 = derive_balance_pda(&program_id, 4, 4, &[0x22u8; 32]);
    let metas_balance = vec![
        AccountMeta::new(authority.pubkey(), true),
        AccountMeta::new_readonly(system_program_id(), false),
        AccountMeta::new(bal_pda_0, false),
        AccountMeta::new(bal_pda_1, false),
    ];
    let ix = Instruction {
        program_id,
        accounts: metas_balance,
        data: build_backfill_balance_data(&balances),
    };
    let sig = send_ix(&rpc, &authority, ix).expect("BackfillBalance tx");
    eprintln!("[ntt-backfill-e2e] BackfillBalance tx={sig}");

    let expected_rent = Rent::default().minimum_balance(BalanceAccountLayout::LEN);
    for (entry, pda) in balances.iter().zip([bal_pda_0, bal_pda_1]) {
        let acc = rpc.get_account(&pda).expect("balance PDA");
        assert_eq!(acc.owner, program_id);
        assert_eq!(acc.data.len(), BalanceAccountLayout::LEN);
        assert_eq!(
            acc.lamports, expected_rent,
            "balance PDA rent should equal `Rent::default().minimum_balance(BalanceAccountLayout::LEN)` ({expected_rent})"
        );
        let layout: &BalanceAccountLayout = bytemuck::from_bytes(&acc.data);
        assert_eq!(layout.chain, entry.chain);
        assert_eq!(layout.token_chain, entry.token_chain);
        assert_eq!(layout.token_address, entry.token_address);
        assert_eq!(layout.balance, Uint256::from_be_bytes(entry.balance));
    }

    // ---------- Phase 3: BackfillRelayerRegistration (2 chains) ----------
    let relayer_entries = [(2u16, [0x33u8; 32]), (5u16, [0x44u8; 32])];
    let relayer_pdas: Vec<Pubkey> = relayer_entries
        .iter()
        .map(|(chain, _)| derive_relayer_pda(&program_id, *chain))
        .collect();
    let mut metas_relayer = vec![
        AccountMeta::new(authority.pubkey(), true),
        AccountMeta::new_readonly(system_program_id(), false),
    ];
    metas_relayer.extend(relayer_pdas.iter().map(|pda| AccountMeta::new(*pda, false)));
    let ix = Instruction {
        program_id,
        accounts: metas_relayer,
        data: build_backfill_relayer_registration_data(&relayer_entries),
    };
    let sig = send_ix(&rpc, &authority, ix).expect("BackfillRelayerRegistration tx");
    eprintln!("[ntt-backfill-e2e] BackfillRelayerRegistration tx={sig}");

    for ((chain, emitter_address), pda) in relayer_entries.iter().zip(&relayer_pdas) {
        let acc = rpc.get_account(pda).expect("relayer registration PDA");
        assert_eq!(acc.owner, program_id);
        assert_eq!(acc.data.len(), RelayerChainRegistrationLayout::LEN);
        let layout: &RelayerChainRegistrationLayout = bytemuck::from_bytes(&acc.data);
        assert_eq!(layout.tag, RelayerChainRegistrationLayout::TAG);
        assert_eq!(layout.chain, *chain);
        assert_eq!(&layout.emitter_address, emitter_address);
    }

    // ---------- Phase 4: BackfillTransceiverHub (1 entry) ----------
    let hub_entry = (2u16, [0x55u8; 32], 7u16, [0x66u8; 32]);
    let hub_pda = derive_hub_pda(&program_id, hub_entry.0, &hub_entry.1);
    let metas_hub = vec![
        AccountMeta::new(authority.pubkey(), true),
        AccountMeta::new_readonly(system_program_id(), false),
        AccountMeta::new(hub_pda, false),
    ];
    let ix = Instruction {
        program_id,
        accounts: metas_hub,
        data: build_backfill_transceiver_hub_data(&[hub_entry]),
    };
    let sig = send_ix(&rpc, &authority, ix).expect("BackfillTransceiverHub tx");
    eprintln!("[ntt-backfill-e2e] BackfillTransceiverHub tx={sig}");

    let acc = rpc.get_account(&hub_pda).expect("transceiver hub PDA");
    assert_eq!(acc.owner, program_id);
    assert_eq!(acc.data.len(), TransceiverHubLayout::LEN);
    let layout: &TransceiverHubLayout = bytemuck::from_bytes(&acc.data);
    assert_eq!(layout.tag, TransceiverHubLayout::TAG);
    assert_eq!(layout.chain, hub_entry.0);
    assert_eq!(layout.address, hub_entry.1);
    assert_eq!(layout.hub_chain, hub_entry.2);
    assert_eq!(layout.hub_address, hub_entry.3);

    // ---------- Phase 5: BackfillTransceiverPeer (1 entry) ----------
    let peer_entry = (2u16, [0x55u8; 32], 9u16, [0x77u8; 32]);
    let peer_pda = derive_peer_pda(&program_id, peer_entry.0, &peer_entry.1, peer_entry.2);
    let metas_peer = vec![
        AccountMeta::new(authority.pubkey(), true),
        AccountMeta::new_readonly(system_program_id(), false),
        AccountMeta::new(peer_pda, false),
    ];
    let ix = Instruction {
        program_id,
        accounts: metas_peer,
        data: build_backfill_transceiver_peer_data(&[peer_entry]),
    };
    let sig = send_ix(&rpc, &authority, ix).expect("BackfillTransceiverPeer tx");
    eprintln!("[ntt-backfill-e2e] BackfillTransceiverPeer tx={sig}");

    let acc = rpc.get_account(&peer_pda).expect("transceiver peer PDA");
    assert_eq!(acc.owner, program_id);
    assert_eq!(acc.data.len(), TransceiverPeerLayout::LEN);
    let layout: &TransceiverPeerLayout = bytemuck::from_bytes(&acc.data);
    assert_eq!(layout.tag, TransceiverPeerLayout::TAG);
    assert_eq!(layout.chain, peer_entry.0);
    assert_eq!(layout.address, peer_entry.1);
    assert_eq!(layout.dest_chain, peer_entry.2);
    assert_eq!(layout.peer_address, peer_entry.3);

    // ---------- Phase 6: wrong-signer BackfillRelayerRegistration → must fail ----------
    //
    // Control: BACKFILL_AUTHORITY is the only gate — a stranger-signed tx
    // must fail with UnauthorizedCaller (Custom(3)).
    let stranger_entry = (9u16, [0x99u8; 32]);
    let stranger_pda = derive_relayer_pda(&program_id, stranger_entry.0);
    let metas_stranger = vec![
        AccountMeta::new(stranger.pubkey(), true),
        AccountMeta::new_readonly(system_program_id(), false),
        AccountMeta::new(stranger_pda, false),
    ];
    let ix = Instruction {
        program_id,
        accounts: metas_stranger,
        data: build_backfill_relayer_registration_data(&[stranger_entry]),
    };
    let err = send_ix(&rpc, &stranger, ix).expect_err("stranger ix must fail");
    let msg = err.to_string();
    eprintln!("[ntt-backfill-e2e] wrong-signer error: {msg}");
    // Custom(3) = UnauthorizedCaller. The RPC client surfaces this as
    // "custom program error: 0x3" in the error string.
    assert!(
        msg.contains("custom program error: 0x3") || msg.contains("Custom(3)"),
        "expected UnauthorizedCaller (Custom(3)) in wrong-signer error, got: {msg}"
    );

    // ---------- Phase 7: WTT authority BackfillRelayerRegistration → must fail ----------
    //
    // Distinct from Phase 6: signs with WTT's real operator authority against
    // the live NTT `.so`, proving cross-program isolation holds end-to-end
    // (see `cross_program_authority_rejected` for the mollusk-level equivalent).
    let wtt_entry = (11u16, [0xAAu8; 32]);
    let wtt_pda = derive_relayer_pda(&program_id, wtt_entry.0);
    let metas_wtt = vec![
        AccountMeta::new(wtt_authority.pubkey(), true),
        AccountMeta::new_readonly(system_program_id(), false),
        AccountMeta::new(wtt_pda, false),
    ];
    let ix = Instruction {
        program_id,
        accounts: metas_wtt,
        data: build_backfill_relayer_registration_data(&[wtt_entry]),
    };
    let err = send_ix(&rpc, &wtt_authority, ix).expect_err("WTT-authority-signed ix must fail");
    let msg = err.to_string();
    eprintln!("[ntt-backfill-e2e] WTT-authority-signed error: {msg}");
    assert!(
        msg.contains("custom program error: 0x3") || msg.contains("Custom(3)"),
        "expected UnauthorizedCaller (Custom(3)) when the WTT authority signs against the NTT \
         program, got: {msg}"
    );
}
