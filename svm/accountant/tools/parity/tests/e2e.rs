//! End-to-end check of the `svm-accountant-parity` binary against a real surfpool
//! validator, for both accountants. Seeds program accounts and noreplay buckets
//! at the build's program addresses to match a small snapshot, asserts a clean
//! strict check, then mutates on-chain state and asserts the exact findings
//! and the non-zero exit. Each test writes its own config file.
//!
//! The checker is read-only, so the e2e seeds raw accounts via
//! `surfnet_setAccount`; the write path (real programs mutating this state)
//! is covered by `tools/backfill`'s own e2e suite.
//!
//! `#[ignore]` by default — spawns surfpool. Run with `just tools-e2e`.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

use global_accountant_definitions::{
    BalanceAccountKey, BalanceAccountLayout, NoReplayBitmapAccount, Uint256, GOVERNANCE_EMITTER,
    NOREPLAY_BITMAP_BYTES, NOREPLAY_PROGRAM_ID, SOLANA_CHAIN_ID,
};
use solana_pubkey::Pubkey;
use svm_accountant_tools_common::accountant::Accountant;
use svm_accountant_tools_common::ids::{Address, ChainId, EmitterKey, Sequence};
use svm_accountant_tools_common::layouts::{
    derive_pda, transceiver_hub_layout, transceiver_peer_layout, PdaAccount,
};
use svm_accountant_tools_common::noreplay::{
    bucket_id, derive_bucket_pda, derive_noreplay_authority_pda,
};
use svm_accountant_tools_common::snapshot::{TransceiverHubRow, TransceiverPeerRow};

use svm_accountant_tools_common::test_support::surfpool::{
    set_account, start_surfpool, SurfpoolOptions,
};

const CHAIN: u16 = 2;
const EMITTER_BYTE: u8 = 0xEE;
const TOKEN_A: u8 = 0xAA;
const TOKEN_B: u8 = 0xAB;
const SEQUENCES: [u64; 3] = [5, 6, 1025];
/// Governance VAA sequences are random 64-bit values.
const GOVERNANCE_SEQUENCE: u64 = 0x0123_4567_89AB_CDEF;

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

fn snapshot_line_governance(sequence: u64) -> String {
    format!(
        r#"{{"kind":"governance","chain":{SOLANA_CHAIN_ID},"emitter":"0x{emitter}","sequence":{sequence},"digest":"0x{digest}"}}"#,
        emitter = hex::encode(GOVERNANCE_EMITTER),
        digest = hex::encode([0xD0; 32]),
    )
}

/// Bucket PDA holding the governance `sequence`.
fn governance_bucket_pda(noreplay_program: &Pubkey, authority: &Pubkey, sequence: u64) -> Pubkey {
    let emitter = EmitterKey {
        chain: ChainId(SOLANA_CHAIN_ID),
        emitter: Address(GOVERNANCE_EMITTER),
    };
    derive_bucket_pda(
        noreplay_program,
        authority,
        &bucket_id(emitter, Sequence(sequence)),
    )
}

/// Balance PDA address and bytes, as the program writes them.
fn balance_layout(program_id: &Pubkey, token_byte: u8, balance_byte: u8) -> (Pubkey, Vec<u8>) {
    let key = BalanceAccountKey::new(CHAIN, CHAIN, [token_byte; 32]);
    let layout =
        BalanceAccountLayout::new(CHAIN, CHAIN, [token_byte; 32], Uint256([balance_byte; 32]));
    (
        derive_pda(program_id, &key),
        bytemuck::bytes_of(&layout).to_vec(),
    )
}

/// Bucket PDA holding `sequence` for `(CHAIN, emitter)`.
fn bucket_pda(
    noreplay_program: &Pubkey,
    authority: &Pubkey,
    emitter: &[u8; 32],
    sequence: u64,
) -> Pubkey {
    let emitter = EmitterKey {
        chain: ChainId(CHAIN),
        emitter: Address(*emitter),
    };
    derive_bucket_pda(
        noreplay_program,
        authority,
        &bucket_id(emitter, Sequence(sequence)),
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

/// Config file with one `[accountant]` table: its snapshot and ledger in `dir`.
fn write_config(dir: &Path, rpc_url: &str, accountant: Accountant) -> PathBuf {
    let path = dir.join("svm-accountant.toml");
    std::fs::write(
        &path,
        format!(
            "rpc_url = \"{rpc_url}\"\n\n[{accountant}]\nsnapshot = \"snapshot.jsonl\"\nledger = \"ledger.jsonl\"\n"
        ),
    )
    .unwrap();
    path
}

struct ParityRun {
    exit_code: i32,
    report: serde_json::Value,
}

/// `check <accountant> <mode...> --out <tmp>`, with the JSON result.
fn run_check(config: &Path, accountant: Accountant, mode: &[&str]) -> ParityRun {
    let out = tempfile::NamedTempFile::new().unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_svm-accountant-parity"))
        .args(["--config", config.to_str().unwrap(), "check"])
        .arg(accountant.to_string())
        .args(mode)
        .args(["--out", out.path().to_str().unwrap()])
        .output()
        .expect("run svm-accountant-parity");
    eprintln!(
        "[parity-e2e] stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    let report: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(out.path()).unwrap()).unwrap();
    ParityRun {
        exit_code: output.status.code().expect("exit code"),
        report,
    }
}

/// `report <accountant>`: exit code and stdout.
fn run_report(config: &Path, accountant: Accountant) -> (Option<i32>, String) {
    let output = Command::new(env!("CARGO_BIN_EXE_svm-accountant-parity"))
        .args(["--config", config.to_str().unwrap(), "report"])
        .arg(accountant.to_string())
        .output()
        .expect("run svm-accountant-parity report");
    (
        output.status.code(),
        String::from_utf8_lossy(&output.stdout).into_owned(),
    )
}

/// Seed the balances and the noreplay buckets of the shared snapshot lines.
fn seed_balances_and_marks(rpc_url: &str, program_id: &Pubkey) -> (Pubkey, Pubkey) {
    let noreplay_program = Pubkey::new_from_array(NOREPLAY_PROGRAM_ID);
    let authority = derive_noreplay_authority_pda(program_id);
    let (balance_a, data_a) = balance_layout(program_id, TOKEN_A, 0x07);
    let (balance_b, data_b) = balance_layout(program_id, TOKEN_B, 0x09);
    set_account(rpc_url, &balance_a, program_id, 10_000_000, &data_a);
    set_account(rpc_url, &balance_b, program_id, 10_000_000, &data_b);

    let emitter = [EMITTER_BYTE; 32];
    let bucket0 = bucket_pda(&noreplay_program, &authority, &emitter, 0);
    let bucket1 = bucket_pda(&noreplay_program, &authority, &emitter, 1024);
    set_account(
        rpc_url,
        &bucket0,
        &noreplay_program,
        10_000_000,
        &bucket_data(&[5, 6]),
    );
    set_account(
        rpc_url,
        &bucket1,
        &noreplay_program,
        10_000_000,
        &bucket_data(&[1]),
    );
    (balance_b, bucket0)
}

fn write_snapshot(dir: &Path, extra_lines: &[String]) {
    let mut file = std::fs::File::create(dir.join("snapshot.jsonl")).unwrap();
    writeln!(file, "{}", snapshot_line_account(TOKEN_A, 0x07)).unwrap();
    writeln!(file, "{}", snapshot_line_account(TOKEN_B, 0x09)).unwrap();
    for sequence in SEQUENCES {
        writeln!(file, "{}", snapshot_line_transfer(sequence)).unwrap();
    }
    writeln!(file, "{}", snapshot_line_governance(GOVERNANCE_SEQUENCE)).unwrap();
    for line in extra_lines {
        writeln!(file, "{line}").unwrap();
    }
}

#[test]
#[ignore = "spawns surfpool; run via `just tools-e2e`"]
fn strict_check_detects_seeded_state_and_mutations() {
    let dir = tempfile::tempdir().unwrap();
    write_snapshot(dir.path(), &[]);

    let guard = start_surfpool(SurfpoolOptions::offline("svm-accountant-parity-e2e"));
    let rpc_url = guard.rpc_url();
    let config = write_config(dir.path(), &rpc_url, Accountant::Wtt);
    let program_id = Accountant::Wtt.program_id();
    let noreplay_program = Pubkey::new_from_array(NOREPLAY_PROGRAM_ID);
    let (balance_b, bucket0) = seed_balances_and_marks(&rpc_url, &program_id);

    let clean = run_check(&config, Accountant::Wtt, &["--strict"]);
    assert_eq!(
        clean.exit_code, 0,
        "seeded state must be clean: {}",
        clean.report
    );
    assert_eq!(clean.report["check"]["clean"], true);
    assert_eq!(clean.report["check"]["program_id"], program_id.to_string());
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
    // extra mark appears inside the covered bucket universe, and the
    // governance sequence is marked.
    let (_, drifted_b) = balance_layout(&program_id, TOKEN_B, 0x0A);
    set_account(&rpc_url, &balance_b, &program_id, 10_000_000, &drifted_b);
    set_account(
        &rpc_url,
        &bucket0,
        &noreplay_program,
        10_000_000,
        &bucket_data(&[5, 7]),
    );
    let governance_bucket = governance_bucket_pda(
        &noreplay_program,
        &derive_noreplay_authority_pda(&program_id),
        GOVERNANCE_SEQUENCE,
    );
    set_account(
        &rpc_url,
        &governance_bucket,
        &noreplay_program,
        10_000_000,
        &bucket_data(&[GOVERNANCE_SEQUENCE]),
    );

    let dirty = run_check(&config, Accountant::Wtt, &["--strict"]);
    assert_eq!(dirty.exit_code, 1, "mutated state must fail");
    let findings = dirty.report["findings"].as_array().unwrap();
    assert_eq!(findings.len(), 4, "one per mutation: {findings:?}");
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
    // Bucket order: the governance emitter's chain 1 sorts before CHAIN.
    assert_eq!(findings[1]["kind"], "replay_marks");
    assert!(findings[1]["key"]
        .as_str()
        .unwrap()
        .contains(&format!("sequence={GOVERNANCE_SEQUENCE}")));
    assert!(findings[1]["wormchain"].is_null());
    assert_eq!(findings[2]["kind"], "replay_marks");
    assert!(findings[2]["key"].as_str().unwrap().contains("sequence=6"));
    assert!(findings[2]["solana"].is_null());
    assert_eq!(findings[3]["kind"], "replay_marks");
    assert!(findings[3]["key"].as_str().unwrap().contains("sequence=7"));
    assert!(findings[3]["wormchain"].is_null());

    // Settle: the mutations are static, so the second sample carries them.
    let settled = run_check(&config, Accountant::Wtt, &["--settle", "1"]);
    assert_eq!(
        settled.exit_code, 1,
        "persistent findings survive the settle margin"
    );
    assert_eq!(settled.report["findings"].as_array().unwrap().len(), 4);

    // Every check appended to the configured ledger; `report` reads the latest.
    let (code, stdout) = run_report(&config, Accountant::Wtt);
    assert_eq!(code, Some(1), "report exits 1 on a dirty check");
    assert!(
        stdout.contains("PARITY: 4 finding(s)"),
        "report reads the check back: {stdout}"
    );
    assert!(
        stdout.contains(&config.display().to_string()),
        "report names the config: {stdout}"
    );
}

/// NTT adds the transceiver maps to the compared state.
#[test]
#[ignore = "spawns surfpool; run via `just tools-e2e`"]
fn ntt_check_compares_the_transceiver_maps() {
    let hub = TransceiverHubRow {
        chain: ChainId(CHAIN),
        address: Address([0x21; 32]),
        hub_chain: ChainId(1),
        hub_address: Address([0x22; 32]),
    };
    let peer = |peer_byte: u8| TransceiverPeerRow {
        chain: ChainId(CHAIN),
        address: Address([0x21; 32]),
        dest_chain: ChainId(4),
        peer_address: Address([peer_byte; 32]),
    };
    let dir = tempfile::tempdir().unwrap();
    write_snapshot(
        dir.path(),
        &[
            format!(
                r#"{{"kind":"transceiver_hub","chain":{CHAIN},"address":"{}","hub_chain":1,"hub_address":"{}"}}"#,
                hub.address, hub.hub_address
            ),
            format!(
                r#"{{"kind":"transceiver_peer","chain":{CHAIN},"address":"{}","dest_chain":4,"peer_address":"{}"}}"#,
                peer(0x31).address,
                peer(0x31).peer_address
            ),
        ],
    );

    let guard = start_surfpool(SurfpoolOptions::offline("svm-accountant-parity-e2e-ntt"));
    let rpc_url = guard.rpc_url();
    let config = write_config(dir.path(), &rpc_url, Accountant::Ntt);
    let program_id = Accountant::Ntt.program_id();
    seed_balances_and_marks(&rpc_url, &program_id);
    let hub_layout = transceiver_hub_layout(&hub);
    set_account(
        &rpc_url,
        &hub_layout.address(&program_id),
        &program_id,
        10_000_000,
        bytemuck::bytes_of(&hub_layout),
    );
    let peer_layout = transceiver_peer_layout(&peer(0x31));
    let peer_pda = peer_layout.address(&program_id);
    set_account(
        &rpc_url,
        &peer_pda,
        &program_id,
        10_000_000,
        bytemuck::bytes_of(&peer_layout),
    );

    let clean = run_check(&config, Accountant::Ntt, &["--strict"]);
    assert_eq!(
        clean.exit_code, 0,
        "seeded state must be clean: {}",
        clean.report
    );
    let states = &clean.report["check"]["states"];
    assert_eq!(states["transceiver_hubs"]["solana_count"], 1);
    assert_eq!(states["transceiver_peers"]["solana_count"], 1);
    assert_eq!(clean.report["check"]["accountant"], "ntt");
    assert_eq!(clean.report["check"]["program_id"], program_id.to_string());

    // The peer now points at another address.
    let moved = transceiver_peer_layout(&peer(0x32));
    assert_eq!(moved.address(&program_id), peer_pda, "same key, same PDA");
    set_account(
        &rpc_url,
        &peer_pda,
        &program_id,
        10_000_000,
        bytemuck::bytes_of(&moved),
    );
    let dirty = run_check(&config, Accountant::Ntt, &["--strict"]);
    assert_eq!(dirty.exit_code, 1, "moved peer must fail");
    let findings = dirty.report["findings"].as_array().unwrap();
    assert_eq!(findings.len(), 1, "{findings:?}");
    assert_eq!(findings[0]["kind"], "transceiver_peers");
    assert!(findings[0]["key"]
        .as_str()
        .unwrap()
        .contains("dest_chain=4"));

    let (code, stdout) = run_report(&config, Accountant::Ntt);
    assert_eq!(code, Some(1), "report exits 1 on a dirty check");
    assert!(stdout.contains("accountant=ntt"), "{stdout}");
}
