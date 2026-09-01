//! Surfpool E2E — backfill program lifecycle against a real subprocess
//! validator, with the production `solana_noreplay.so` co-deployed.
//!
//! Phases the test exercises in order:
//!
//! 1. `BackfillNoReplay` for three entries spanning two buckets. Assert the
//!    NoReplay bitmap bits flip and the canonical `ACCDGST\0` commit-log
//!    entries appear via `meta.logMessages` over real RPC.
//! 2. `BackfillBalance` for two accounts. Assert both `BalanceAccountLayout`
//!    PDAs are written at canonical seeds via `getAccountInfo`.
//! 3. `BackfillNoReplay` signed by a non-authority keypair. Assert the tx
//!    fails with `UnauthorizedCaller` (Custom(3)), confirming the
//!    compile-time `BACKFILL_AUTHORITY` const gate runs on-chain.
//! 4. `BackfillBalance` signed by the same non-authority keypair. Assert
//!    the same `UnauthorizedCaller` (Custom(3)) failure, and that
//!    `getAccountInfo` on the target PDA still errors, confirming the
//!    authority gate covers `BackfillBalance`'s own handler too.

#![allow(clippy::too_many_arguments)]

use std::{str::FromStr, time::Duration};

use solana_client::{client_error::ClientError, rpc_client::RpcClient};
use solana_instruction::{AccountMeta, Instruction};
use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use solana_rent::Rent;
use solana_signer::Signer;
use solana_transaction::Transaction;

use global_accountant_backfill::Instruction as IxDiscriminator;
use global_accountant_definitions::{
    BalanceAccountLayout, NoReplayBitmapAccount, Uint256, ACCOUNT_SEED_PREFIX,
    NOREPLAY_AUTHORITY_SEED_PREFIX, NOREPLAY_PROGRAM_ID,
};

mod common;
use common::surfpool::{
    await_confirmed, deploy_program, fetch_accdgst_logs, so_path, start_surfpool, SurfpoolOptions,
};

const BACKFILL_PROGRAM_NAME: &str = "global_accountant_backfill";

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
    let bucket_index = NoReplayBitmapAccount::bucket_index(sequence).to_le_bytes();
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

fn system_program_id() -> Pubkey {
    Pubkey::from_str("11111111111111111111111111111111").unwrap()
}

fn send_ix(rpc: &RpcClient, payer: &Keypair, ix: Instruction) -> Result<String, ClientError> {
    let blockhash = rpc.get_latest_blockhash()?;
    let tx = Transaction::new_signed_with_payer(
        &[ix],
        Some(&payer.pubkey()),
        &[payer],
        blockhash,
    );
    rpc.send_and_confirm_transaction(&tx).map(|s| s.to_string())
}

// ============================================================================
// The test
// ============================================================================

#[test]
#[ignore = "spawns surfpool subprocess; run via `cargo test --test surfpool_e2e_backfill_lifecycle -- --ignored`"]
fn surfpool_backfill_lifecycle() {
    // ---------- Load .so artefacts ----------
    let backfill_so = so_path(BACKFILL_PROGRAM_NAME);
    let backfill_bytes = std::fs::read(&backfill_so).unwrap_or_else(|e| {
        panic!(
            "could not read {}: {e}. Run `just build` first.",
            backfill_so.display()
        )
    });
    let noreplay_bytes = accountant_test_fixtures::NOREPLAY_SO.bytes;
    eprintln!(
        "[backfill-e2e] backfill={} bytes, noreplay={} bytes",
        backfill_bytes.len(),
        noreplay_bytes.len()
    );

    // ---------- Boot surfpool offline ----------
    let guard = start_surfpool(SurfpoolOptions::offline("ga-backfill-e2e"));
    let rpc_url = guard.rpc_url();
    let rpc = guard.rpc_client();

    // ---------- Fixed program id + authority/payer airdrop ----------
    //
    // Anchor's `declare_id!` pins this program to a single fixed address,
    // checked on every entry (`DeclaredProgramIdMismatch` otherwise), so
    // deploy at that fixed ID.
    let program_id = Pubkey::new_from_array(global_accountant_backfill::ID.to_bytes());
    // Payer must equal `BACKFILL_AUTHORITY` — deterministic test keypair
    // (seed `[1u8; 32]`) keeps fixtures reproducible.
    let authority = Keypair::new_from_array([1u8; 32]);
    let stranger = Keypair::new();
    eprintln!(
        "[backfill-e2e] program_id={program_id} authority={} stranger={}",
        authority.pubkey(),
        stranger.pubkey()
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

    deploy_program(&rpc_url, &program_id, &backfill_bytes);
    deploy_program(
        &rpc_url,
        &Pubkey::new_from_array(NOREPLAY_PROGRAM_ID),
        noreplay_bytes,
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
    eprintln!("[backfill-e2e] BackfillNoReplay tx={sig}");

    // Three ACCDGST\0 log entries in original order.
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

    // Bitmap bits set in both buckets.
    let b0 = rpc.get_account(&bucket_0).expect("bucket_0 account");
    let b1 = rpc.get_account(&bucket_1).expect("bucket_1 account");
    let bitmap_0 = NoReplayBitmapAccount::from_bytes(&b0.data).expect("bucket_0 bitmap");
    let bitmap_1 = NoReplayBitmapAccount::from_bytes(&b1.data).expect("bucket_1 bitmap");
    for seq in [10u64, 500] {
        assert!(bitmap_0.is_marked(seq), "expected bit for {seq} set");
    }
    assert!(bitmap_1.is_marked(1500), "expected bit for 1500 set");

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
    eprintln!("[backfill-e2e] BackfillBalance tx={sig}");

    // Pin the rent-exempt minimum the runtime debits per balance PDA. A
    // regression in `BalanceAccountLayout` size (adding a field, restoring
    // `_reserved`) would shift this number and trip the assert before any
    // mainnet rent estimate goes stale.
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

    // ---------- Phase 3: wrong-signer BackfillNoReplay → must fail ----------
    //
    // The compile-time `BACKFILL_AUTHORITY` const gates arbitrary state
    // writes. Run a control tx signed by a stranger and assert
    // `UnauthorizedCaller` (Custom(3)).
    let stranger_entry = NoReplayEntry {
        chain: 2,
        emitter,
        sequence: 9999,
        digest: [0xb0u8; 32],
    };
    let stranger_bucket = derive_noreplay_bucket(&noreplay_auth_pda, 2, &emitter, 9999);
    let metas_stranger = vec![
        AccountMeta::new(stranger.pubkey(), true),
        AccountMeta::new_readonly(Pubkey::new_from_array(NOREPLAY_PROGRAM_ID), false),
        AccountMeta::new_readonly(noreplay_auth_pda, false),
        AccountMeta::new_readonly(system_program_id(), false),
        AccountMeta::new(stranger_bucket, false),
    ];
    let ix = Instruction {
        program_id,
        accounts: metas_stranger,
        data: build_backfill_noreplay_data(&[stranger_entry]),
    };
    let err = send_ix(&rpc, &stranger, ix).expect_err("stranger ix must fail");
    let msg = err.to_string();
    eprintln!("[backfill-e2e] wrong-signer error: {msg}");
    // Custom(3) = UnauthorizedCaller. The RPC client surfaces this as
    // "custom program error: 0x3" in the error string.
    assert!(
        msg.contains("custom program error: 0x3") || msg.contains("Custom(3)"),
        "expected UnauthorizedCaller (Custom(3)) in wrong-signer error, got: {msg}"
    );

    // ---------- Phase 4: wrong-signer BackfillBalance → must fail ----------
    //
    // `BackfillBalance` shares `require_authority` but is a separate
    // handler; run the same control here too. Fresh (chain, token_chain,
    // token_address) key avoids colliding with the PDA written in Phase 2.
    let stranger_balance = BalanceEntry {
        chain: 9,
        token_chain: 9,
        token_address: [0x99u8; 32],
        balance: {
            let mut b = [0u8; 32];
            b[24..32].copy_from_slice(&1u64.to_be_bytes());
            b
        },
    };
    let stranger_bal_pda = derive_balance_pda(&program_id, 9, 9, &[0x99u8; 32]);
    let metas_stranger_balance = vec![
        AccountMeta::new(stranger.pubkey(), true),
        AccountMeta::new_readonly(system_program_id(), false),
        AccountMeta::new(stranger_bal_pda, false),
    ];
    let ix = Instruction {
        program_id,
        accounts: metas_stranger_balance,
        data: build_backfill_balance_data(&[stranger_balance]),
    };
    let err = send_ix(&rpc, &stranger, ix).expect_err("stranger BackfillBalance ix must fail");
    let msg = err.to_string();
    eprintln!("[backfill-e2e] BackfillBalance wrong-signer error: {msg}");
    assert!(
        msg.contains("custom program error: 0x3") || msg.contains("Custom(3)"),
        "expected UnauthorizedCaller (Custom(3)) in BackfillBalance wrong-signer error, got: {msg}"
    );
    assert!(
        rpc.get_account(&stranger_bal_pda).is_err(),
        "stranger's BackfillBalance PDA must not exist after a rejected tx"
    );
}
