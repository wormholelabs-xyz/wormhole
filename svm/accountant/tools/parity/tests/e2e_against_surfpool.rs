//! End-to-end check of the `ga-parity` binary against a real surfpool
//! validator. Seeds balance PDAs and noreplay buckets to match a small
//! snapshot, asserts a clean strict check, then mutates on-chain state and
//! asserts the exact findings and the non-zero exit.
//!
//! The checker is read-only, so the e2e seeds raw accounts via
//! `surfnet_setAccount`; the write path (real programs mutating this state)
//! is covered by `tools/backfill`'s own e2e suite.
//!
//! `#[ignore]` by default — spawns surfpool. Run with `just tools-e2e`.

use std::io::Write;
use std::process::Command;

use ga_backfill::expected_accounts::derive_pda;
use ga_backfill::noreplay::{derive_noreplay_authority_pda, derive_noreplay_bucket};
use global_accountant_definitions::{
    BalanceAccountKey, BalanceAccountLayout, NoReplayBitmapAccount, Uint256, NOREPLAY_BITMAP_BYTES,
};
use solana_pubkey::Pubkey;

mod common;
use common::surfpool::{set_account, start_surfpool};

const CHAIN: u16 = 2;
const EMITTER_BYTE: u8 = 0xEE;
const TOKEN_A: u8 = 0xAA;
const TOKEN_B: u8 = 0xAB;
const SEQUENCES: [u64; 3] = [5, 6, 1025];

fn snapshot_line_account(token_byte: u8, balance_byte: u8) -> String {
    format!(
        r#"{{"kind":"account","chain":{CHAIN},"token_chain":{CHAIN},"token_address":"0x{token}","balance":"0x{balance}"}}"#,
        token = hex::encode([token_byte; 32]),
        balance = hex::encode([balance_byte; 32]),
    )
}

fn snapshot_line_transfer(sequence: u64) -> String {
    format!(
        r#"{{"kind":"transfer","chain":{CHAIN},"emitter":"0x{emitter}","sequence":{sequence},"digest":"0x{digest}","amount":"0x{amount}","token_chain":{CHAIN},"token_address":"0x{token}","recipient_chain":1}}"#,
        emitter = hex::encode([EMITTER_BYTE; 32]),
        digest = hex::encode([0xDD; 32]),
        amount = hex::encode([0x01; 32]),
        token = hex::encode([TOKEN_A; 32]),
    )
}

/// Balance PDA address and bytes, as the program writes them.
fn balance_account(program_id: &Pubkey, token_byte: u8, balance_byte: u8) -> (Pubkey, Vec<u8>) {
    let key = BalanceAccountKey::new(CHAIN, CHAIN, [token_byte; 32]);
    let layout =
        BalanceAccountLayout::new(CHAIN, CHAIN, [token_byte; 32], Uint256([balance_byte; 32]));
    (
        derive_pda(program_id, &key),
        bytemuck::bytes_of(&layout).to_vec(),
    )
}

/// Bucket account: bump byte then the 128-byte bitmap.
fn bucket_data(bits: &[u64]) -> Vec<u8> {
    let mut bucket = NoReplayBitmapAccount {
        bump: 0xFF,
        bitmap: [0; NOREPLAY_BITMAP_BYTES],
    };
    for &bit in bits {
        let bit = NoReplayBitmapAccount::bit_index(bit);
        bucket.bitmap[bit / 8] |= 1 << (bit % 8);
    }
    bytemuck::bytes_of(&bucket).to_vec()
}

struct ParityRun {
    exit_code: i32,
    report: serde_json::Value,
}

fn run_parity(
    snapshot: &std::path::Path,
    rpc_url: &str,
    program_id: &Pubkey,
    noreplay_program: &Pubkey,
    extra: &[&str],
) -> ParityRun {
    let out = tempfile::NamedTempFile::new().unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_ga-parity"))
        .args([
            "check",
            "--snapshot",
            snapshot.to_str().unwrap(),
            "--accountant",
            "wtt",
            "--program-id",
            &program_id.to_string(),
            "--noreplay-program-id",
            &noreplay_program.to_string(),
            "--rpc-url",
            rpc_url,
            "--out",
            out.path().to_str().unwrap(),
        ])
        .args(extra)
        .output()
        .expect("run ga-parity");
    eprintln!(
        "[parity-e2e] stdout:\n{}",
        String::from_utf8_lossy(&output.stdout),
    );
    let report: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(out.path()).unwrap()).unwrap();
    ParityRun {
        exit_code: output.status.code().expect("exit code"),
        report,
    }
}

#[test]
#[ignore = "spawns surfpool; run via `just tools-e2e`"]
fn strict_check_detects_seeded_state_and_mutations() {
    let dir = tempfile::tempdir().unwrap();
    let snapshot = dir.path().join("catalogue.jsonl");
    {
        let mut file = std::fs::File::create(&snapshot).unwrap();
        writeln!(file, "{}", snapshot_line_account(TOKEN_A, 0x07)).unwrap();
        writeln!(file, "{}", snapshot_line_account(TOKEN_B, 0x09)).unwrap();
        for sequence in SEQUENCES {
            writeln!(file, "{}", snapshot_line_transfer(sequence)).unwrap();
        }
    }

    let guard = start_surfpool("ga-parity-e2e");
    let rpc_url = guard.rpc_url();
    let program_id = Pubkey::new_unique();
    let noreplay_program = Pubkey::new_unique();
    let authority = derive_noreplay_authority_pda(&program_id);

    let (balance_a, data_a) = balance_account(&program_id, TOKEN_A, 0x07);
    let (balance_b, data_b) = balance_account(&program_id, TOKEN_B, 0x09);
    set_account(&rpc_url, &balance_a, &program_id, 10_000_000, &data_a);
    set_account(&rpc_url, &balance_b, &program_id, 10_000_000, &data_b);

    let emitter = [EMITTER_BYTE; 32];
    let bucket0 = derive_noreplay_bucket(&noreplay_program, &authority, CHAIN, &emitter, 0);
    let bucket1 = derive_noreplay_bucket(&noreplay_program, &authority, CHAIN, &emitter, 1024);
    set_account(
        &rpc_url,
        &bucket0,
        &noreplay_program,
        10_000_000,
        &bucket_data(&[5, 6]),
    );
    set_account(
        &rpc_url,
        &bucket1,
        &noreplay_program,
        10_000_000,
        &bucket_data(&[1]),
    );

    let clean = run_parity(
        &snapshot,
        &rpc_url,
        &program_id,
        &noreplay_program,
        &["--strict"],
    );
    assert_eq!(
        clean.exit_code, 0,
        "seeded state must be clean: {}",
        clean.report
    );
    assert_eq!(clean.report["check"]["clean"], true);
    let states = &clean.report["check"]["states"];
    assert_eq!(states["balances"]["solana_count"], 2);
    assert_eq!(states["replay_marks"]["solana_count"], 3);
    assert_eq!(
        states["balances"]["wormchain_hash"], states["balances"]["solana_hash"],
        "balance hashes agree on clean state",
    );
    assert_eq!(
        states["replay_marks"]["wormchain_hash"], states["replay_marks"]["solana_hash"],
        "replay hashes agree on clean state",
    );

    // Mutations: one balance value drifts, one replay mark disappears, one
    // extra mark appears inside the covered bucket universe.
    let (_, drifted_b) = balance_account(&program_id, TOKEN_B, 0x0A);
    set_account(&rpc_url, &balance_b, &program_id, 10_000_000, &drifted_b);
    set_account(
        &rpc_url,
        &bucket0,
        &noreplay_program,
        10_000_000,
        &bucket_data(&[5, 7]),
    );

    let dirty = run_parity(
        &snapshot,
        &rpc_url,
        &program_id,
        &noreplay_program,
        &["--strict"],
    );
    assert_eq!(dirty.exit_code, 1, "mutated state must fail");
    let findings = dirty.report["findings"].as_array().unwrap();
    assert_eq!(findings.len(), 3, "one per mutation: {findings:?}");
    assert_eq!(findings[0]["kind"], "balances");
    assert!(findings[0]["key"]
        .as_str()
        .unwrap()
        .contains(&hex::encode([TOKEN_B; 32])));
    assert!(findings[0]["wormchain"]
        .as_str()
        .unwrap()
        .contains(&hex::encode([0x09u8])));
    assert!(findings[0]["solana"]
        .as_str()
        .unwrap()
        .contains(&hex::encode([0x0Au8])));
    assert_eq!(findings[1]["kind"], "replay_marks");
    assert!(findings[1]["key"].as_str().unwrap().contains("sequence=6"));
    assert!(findings[1]["solana"].is_null());
    assert_eq!(findings[2]["kind"], "replay_marks");
    assert!(findings[2]["key"].as_str().unwrap().contains("sequence=7"));
    assert!(findings[2]["wormchain"].is_null());

    // Settle: the mutations are static, so the second sample carries them.
    let settled = run_parity(
        &snapshot,
        &rpc_url,
        &program_id,
        &noreplay_program,
        &["--settle", "1"],
    );
    assert_eq!(
        settled.exit_code, 1,
        "persistent findings survive the settle margin"
    );
    assert_eq!(settled.report["findings"].as_array().unwrap().len(), 3);

    // Ledger + report round-trip.
    let ledger = dir.path().join("ledger.jsonl");
    let logged = Command::new(env!("CARGO_BIN_EXE_ga-parity"))
        .args([
            "check",
            "--snapshot",
            snapshot.to_str().unwrap(),
            "--accountant",
            "wtt",
            "--program-id",
            &program_id.to_string(),
            "--noreplay-program-id",
            &noreplay_program.to_string(),
            "--rpc-url",
            &rpc_url,
            "--strict",
            "--ledger",
            ledger.to_str().unwrap(),
        ])
        .output()
        .expect("run ga-parity with ledger");
    assert_eq!(logged.status.code(), Some(1));

    let report = Command::new(env!("CARGO_BIN_EXE_ga-parity"))
        .args(["report", "--ledger", ledger.to_str().unwrap()])
        .output()
        .expect("run ga-parity report");
    assert_eq!(
        report.status.code(),
        Some(1),
        "report exits 1 on a dirty check"
    );
    let stdout = String::from_utf8_lossy(&report.stdout);
    assert!(
        stdout.contains("PARITY: 3 finding(s)"),
        "report replays the check: {stdout}"
    );
}
