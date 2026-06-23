//! **This is an operator cost-measurement tool, NOT a regression test.**
//!
//! It lives under `tests/` because the `#[ignore]` + `cargo test --ignored`
//! workflow is the cheapest way to run it, but it makes no behavioural
//! assertions worth gating CI on. The asserts at the end (`total_failures_v
//! == 0`, `rent_per_bucket > 0`) are sanity-checks against the probe itself,
//! not against the program.
//!
//! What it does: drives `TRANSFER_SAMPLE` transfer entries and `ACCOUNT_SAMPLE`
//! account entries from `/tmp/wormchain-mainnet-snapshot/catalogue.jsonl`
//! through the backfill program against a real surfpool subprocess. Captures
//! per-tx `fee`, `computeUnitsConsumed`, and `(preBalance - postBalance - fee)`
//! (the rent debit) via `getTransaction`; averages per-entry, extrapolates to
//! the full mainnet catalogue (5.5M transfers + 17K accounts + 40
//! registrations + 6 modifications), and prints a SOL / USD breakdown.
//!
//! Requires `/tmp/wormchain-mainnet-snapshot/catalogue.jsonl` to exist — not
//! present on CI or a reviewer's machine. The `_at_scale` sibling runs a
//! larger sample (100k transfers, parallelised) and is what the master plan's
//! cost projection is anchored on; this smaller probe is the quick-feedback
//! version for iterating on the orchestrator.
//!
//! Run via:
//!   `cargo test --test surfpool_e2e_cost_probe -- --ignored --nocapture`

#![allow(clippy::too_many_arguments)]

use std::{
    fs::File,
    io::{BufRead, BufReader},
    str::FromStr,
    thread,
    time::{Duration, Instant},
};

use serde_json::json;
use solana_client::{client_error::ClientError, rpc_client::RpcClient};
use solana_instruction::{AccountMeta, Instruction};
use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use solana_signer::Signer;
use solana_transaction::Transaction;

use global_accountant_backfill::Instruction as IxDiscriminator;
use global_accountant_definitions::{
    ACCOUNT_SEED_PREFIX, NOREPLAY_AUTHORITY_SEED_PREFIX, NOREPLAY_BITS_PER_BUCKET,
    NOREPLAY_PROGRAM_ID,
};

mod common;
use common::surfpool::{deploy_program, rpc_call, so_path, start_surfpool, SurfpoolOptions};

const BACKFILL_PROGRAM_NAME: &str = "global_accountant_backfill";
const CATALOGUE_PATH: &str = "/tmp/wormchain-mainnet-snapshot/catalogue.jsonl";

// Full mainnet catalogue totals as of snapshot at height 18,669,029.
const FULL_TRANSFER_COUNT: u64 = 5_516_669;
const FULL_ACCOUNT_COUNT: u64 = 17_367;
const FULL_REGISTRATION_COUNT: u64 = 40;
const FULL_MODIFICATION_COUNT: u64 = 6;

// Sample sizes. Kept small so the probe finishes in seconds; large enough that
// per-tx variance washes out via averaging.
const TRANSFER_SAMPLE: usize = 100;
const ACCOUNT_SAMPLE: usize = 50;

// Batch sizes per tx. The wire-size budget (1232 bytes per tx) caps us around
// here once metas + ix data are accounted for. Picked empirically just below
// the upper bound to leave header headroom.
const TRANSFER_BATCH: usize = 10;
const ACCOUNT_BATCH: usize = 8;

// Headline conversion. The probe prints all numbers in both lamports and
// dollars; this only affects the human-friendly figure.
const SOL_USD: f64 = 230.0;
const LAMPORTS_PER_SOL: f64 = 1_000_000_000.0;

#[derive(Clone, Copy, Debug)]
struct TransferEntry {
    chain: u16,
    emitter: [u8; 32],
    sequence: u64,
    digest: [u8; 32],
}

#[derive(Clone, Copy, Debug)]
struct AccountEntry {
    chain: u16,
    token_chain: u16,
    token_address: [u8; 32],
    balance: [u8; 32],
}

#[derive(Default, Debug)]
struct TxMeasurement {
    fee_lamports: u64,
    cu_consumed: u64,
    rent_lamports: u64,
    entry_count: usize,
}

fn parse_hex32(s: &str) -> [u8; 32] {
    let s = s.strip_prefix("0x").unwrap_or(s);
    let bytes = hex_decode(s);
    assert_eq!(bytes.len(), 32, "expected 32 bytes, got {}", bytes.len());
    let mut out = [0u8; 32];
    out.copy_from_slice(&bytes);
    out
}

fn hex_decode(s: &str) -> Vec<u8> {
    assert_eq!(s.len() % 2, 0, "odd hex length");
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("hex char"))
        .collect()
}

fn read_samples() -> (Vec<TransferEntry>, Vec<AccountEntry>) {
    let f = File::open(CATALOGUE_PATH).unwrap_or_else(|e| {
        panic!(
            "missing catalogue at {CATALOGUE_PATH}: {e}\nRun the snapshot tool first."
        )
    });
    let reader = BufReader::new(f);
    let mut transfers: Vec<TransferEntry> = Vec::with_capacity(TRANSFER_SAMPLE);
    let mut accounts: Vec<AccountEntry> = Vec::with_capacity(ACCOUNT_SAMPLE);
    for line in reader.lines() {
        let line = line.expect("read line");
        if line.is_empty() {
            continue;
        }
        let v: serde_json::Value = serde_json::from_str(&line).expect("parse JSON");
        let kind = v.get("kind").and_then(|k| k.as_str()).unwrap_or("");
        match kind {
            "transfer" if transfers.len() < TRANSFER_SAMPLE => {
                transfers.push(TransferEntry {
                    chain: v["chain"].as_u64().expect("chain") as u16,
                    emitter: parse_hex32(v["emitter"].as_str().expect("emitter")),
                    sequence: v["sequence"].as_u64().expect("sequence"),
                    digest: parse_hex32(v["digest"].as_str().expect("digest")),
                });
            }
            "account" if accounts.len() < ACCOUNT_SAMPLE => {
                accounts.push(AccountEntry {
                    chain: v["chain"].as_u64().expect("chain") as u16,
                    token_chain: v["token_chain"].as_u64().expect("token_chain") as u16,
                    token_address: parse_hex32(v["token_address"].as_str().expect("addr")),
                    balance: parse_hex32(v["balance"].as_str().expect("balance")),
                });
            }
            _ => {}
        }
        if transfers.len() >= TRANSFER_SAMPLE && accounts.len() >= ACCOUNT_SAMPLE {
            break;
        }
    }
    assert!(
        !transfers.is_empty() && !accounts.is_empty(),
        "could not collect samples; sample limits were {TRANSFER_SAMPLE} / {ACCOUNT_SAMPLE}"
    );
    (transfers, accounts)
}

fn system_program_id() -> Pubkey {
    Pubkey::from_str("11111111111111111111111111111111").unwrap()
}

fn derive_noreplay_authority_pda(program_id: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(&[NOREPLAY_AUTHORITY_SEED_PREFIX], program_id).0
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
    Pubkey::find_program_address(
        &[
            authority.as_ref(),
            &namespace[..32],
            &namespace[32..],
            &bucket_index,
        ],
        &Pubkey::new_from_array(NOREPLAY_PROGRAM_ID),
    )
    .0
}

fn derive_balance_pda(program_id: &Pubkey, chain: u16, token_chain: u16, token_address: &[u8; 32]) -> Pubkey {
    Pubkey::find_program_address(
        &[
            ACCOUNT_SEED_PREFIX,
            &chain.to_be_bytes(),
            &token_chain.to_be_bytes(),
            token_address,
        ],
        program_id,
    )
    .0
}

fn build_backfill_noreplay_ix(
    program_id: &Pubkey,
    payer: &Pubkey,
    noreplay_auth: Pubkey,
    chunk: &[TransferEntry],
) -> Instruction {
    // Compact emitter-grouped wire:
    // [disc][group_count] [chain emitter entry_count [seq digest]...]...
    let mut groups: Vec<Vec<TransferEntry>> = Vec::new();
    let mut current: Vec<TransferEntry> = Vec::new();
    let mut current_key: Option<(u16, [u8; 32])> = None;
    for e in chunk {
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

    // Unique buckets in order of first occurrence; the handler walks entries
    // in lockstep with this slot list, flushing on bucket transition.
    let mut bucket_metas: Vec<AccountMeta> = Vec::new();
    let mut prev_bucket: Option<(u16, [u8; 32], u64)> = None;
    for e in chunk {
        let bucket_idx = e.sequence / NOREPLAY_BITS_PER_BUCKET;
        let cur = (e.chain, e.emitter, bucket_idx);
        if Some(cur) != prev_bucket {
            let pda = derive_noreplay_bucket(&noreplay_auth, e.chain, &e.emitter, e.sequence);
            bucket_metas.push(AccountMeta::new(pda, false));
            prev_bucket = Some(cur);
        }
    }

    let mut metas = vec![
        AccountMeta::new(*payer, true),
        AccountMeta::new_readonly(Pubkey::new_from_array(NOREPLAY_PROGRAM_ID), false),
        AccountMeta::new_readonly(noreplay_auth, false),
        AccountMeta::new_readonly(system_program_id(), false),
    ];
    metas.extend(bucket_metas);
    Instruction {
        program_id: *program_id,
        accounts: metas,
        data,
    }
}

fn build_backfill_balance_ix(
    program_id: &Pubkey,
    payer: &Pubkey,
    chunk: &[AccountEntry],
) -> Instruction {
    let mut data = Vec::with_capacity(2 + chunk.len() * 68);
    data.push(IxDiscriminator::BackfillBalance as u8);
    data.push(chunk.len() as u8);
    for e in chunk {
        data.extend_from_slice(&e.chain.to_be_bytes());
        data.extend_from_slice(&e.token_chain.to_be_bytes());
        data.extend_from_slice(&e.token_address);
        data.extend_from_slice(&e.balance);
    }
    let mut metas = vec![
        AccountMeta::new(*payer, true),
        AccountMeta::new_readonly(system_program_id(), false),
    ];
    for e in chunk {
        let pda = derive_balance_pda(program_id, e.chain, e.token_chain, &e.token_address);
        metas.push(AccountMeta::new(pda, false));
    }
    Instruction {
        program_id: *program_id,
        accounts: metas,
        data,
    }
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

fn fetch_meta(rpc_url: &str, sig: &str) -> TxMeasurement {
    // getTransaction can lag a slot post-confirmation; brief retry.
    let deadline = Instant::now() + Duration::from_secs(8);
    let mut last = serde_json::Value::Null;
    while Instant::now() < deadline {
        last = rpc_call(
            rpc_url,
            "getTransaction",
            json!([
                sig,
                {
                    "encoding": "json",
                    "commitment": "confirmed",
                    "maxSupportedTransactionVersion": 0,
                }
            ]),
        );
        if last.get("result").is_some_and(|v| !v.is_null()) {
            break;
        }
        thread::sleep(Duration::from_millis(120));
    }
    let meta = &last["result"]["meta"];
    let fee = meta["fee"].as_u64().expect("fee");
    let cu = meta["computeUnitsConsumed"].as_u64().unwrap_or(0);
    let pre = meta["preBalances"][0].as_u64().expect("pre");
    let post = meta["postBalances"][0].as_u64().expect("post");
    // Payer is account index 0 (sole signer + writable fee-payer). The drop
    // beyond `fee` is rent debited for any PDAs the tx created.
    let rent = pre.saturating_sub(post + fee);
    TxMeasurement {
        fee_lamports: fee,
        cu_consumed: cu,
        rent_lamports: rent,
        entry_count: 0,
    }
}

#[derive(Default, Debug)]
struct Aggregate {
    txs: u64,
    entries: u64,
    fee_lamports: u64,
    cu_consumed: u64,
    rent_lamports: u64,
}

impl Aggregate {
    fn add(&mut self, m: TxMeasurement) {
        self.txs += 1;
        self.entries += m.entry_count as u64;
        self.fee_lamports += m.fee_lamports;
        self.cu_consumed += m.cu_consumed;
        self.rent_lamports += m.rent_lamports;
    }
}

fn lamports_to_usd(lamports: u64) -> f64 {
    (lamports as f64 / LAMPORTS_PER_SOL) * SOL_USD
}

#[test]
#[ignore = "spawns surfpool; reads /tmp/wormchain-mainnet-snapshot/catalogue.jsonl; run with --ignored"]
fn surfpool_cost_probe() {
    // -------- Load samples from the real catalogue --------
    let (mut transfers, mut accounts) = read_samples();
    eprintln!(
        "[cost-probe] loaded {} transfer + {} account samples from {CATALOGUE_PATH}",
        transfers.len(),
        accounts.len()
    );

    // Strict-ascending sort required by the program.
    transfers.sort_by_key(|e| (e.chain, e.emitter, e.sequence));
    transfers.dedup_by_key(|e| (e.chain, e.emitter, e.sequence));
    accounts.sort_by_key(|e| (e.chain, e.token_chain, e.token_address));
    accounts.dedup_by_key(|e| (e.chain, e.token_chain, e.token_address));

    // -------- Boot surfpool + deploy --------
    let backfill_so = so_path(BACKFILL_PROGRAM_NAME);
    let backfill_bytes = std::fs::read(&backfill_so).unwrap_or_else(|e| {
        panic!("read {}: {e} — run `just build` first", backfill_so.display())
    });
    let noreplay_so = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/solana_noreplay.so");
    let noreplay_bytes = std::fs::read(&noreplay_so).expect("noreplay fixture");

    let guard = start_surfpool(SurfpoolOptions::offline("backfill-cost-probe"));
    let rpc_url = guard.rpc_url();
    let rpc = guard.rpc_client();

    // Payer must equal `BACKFILL_AUTHORITY` (test default: `Keypair::new_from_array([1u8; 32])`).
    let payer = Keypair::new_from_array([1u8; 32]);
    rpc.request_airdrop(&payer.pubkey(), 1_000_000_000_000)
        .expect("airdrop");
    // Brief wait for airdrop to land.
    thread::sleep(Duration::from_millis(300));

    let program_id = Keypair::new().pubkey();
    deploy_program(&rpc_url, &program_id, &backfill_bytes);
    deploy_program(
        &rpc_url,
        &Pubkey::new_from_array(NOREPLAY_PROGRAM_ID),
        &noreplay_bytes,
    );

    let noreplay_auth = derive_noreplay_authority_pda(&program_id);

    // -------- Drive transfer batches --------
    let mut transfer_agg = Aggregate::default();
    for chunk in transfers.chunks(TRANSFER_BATCH) {
        let ix = build_backfill_noreplay_ix(
            &program_id,
            &payer.pubkey(),
            noreplay_auth,
            chunk,
        );
        let sig = send_ix(&rpc, &payer, ix).expect("send BackfillNoReplay");
        let mut m = fetch_meta(&rpc_url, &sig);
        m.entry_count = chunk.len();
        eprintln!(
            "[cost-probe] NoReplay tx={}, entries={}, fee={}L, cu={}, rent={}L",
            &sig[..8],
            m.entry_count,
            m.fee_lamports,
            m.cu_consumed,
            m.rent_lamports
        );
        transfer_agg.add(m);
    }

    // -------- Drive account batches --------
    let mut balance_agg = Aggregate::default();
    for chunk in accounts.chunks(ACCOUNT_BATCH) {
        let ix = build_backfill_balance_ix(&program_id, &payer.pubkey(), chunk);
        let sig = send_ix(&rpc, &payer, ix).expect("send BackfillBalance");
        let mut m = fetch_meta(&rpc_url, &sig);
        m.entry_count = chunk.len();
        eprintln!(
            "[cost-probe] Balance  tx={}, entries={}, fee={}L, cu={}, rent={}L",
            &sig[..8],
            m.entry_count,
            m.fee_lamports,
            m.cu_consumed,
            m.rent_lamports
        );
        balance_agg.add(m);
    }

    // -------- Extrapolate + print --------
    let tx_full_transfer = (FULL_TRANSFER_COUNT as f64 / transfer_agg.entries as f64
        * transfer_agg.txs as f64)
        .ceil() as u64;
    let tx_full_balance = (FULL_ACCOUNT_COUNT as f64 / balance_agg.entries as f64
        * balance_agg.txs as f64)
        .ceil() as u64;

    let scale_t = FULL_TRANSFER_COUNT as f64 / transfer_agg.entries as f64;
    let scale_a = FULL_ACCOUNT_COUNT as f64 / balance_agg.entries as f64;

    let t_fee = (transfer_agg.fee_lamports as f64 * scale_t) as u64;
    let t_rent = (transfer_agg.rent_lamports as f64 * scale_t) as u64;
    let a_fee = (balance_agg.fee_lamports as f64 * scale_a) as u64;
    let a_rent = (balance_agg.rent_lamports as f64 * scale_a) as u64;

    let total_fee = t_fee + a_fee;
    let total_rent = t_rent + a_rent;
    let grand = total_fee + total_rent;

    eprintln!();
    eprintln!("============================================================");
    eprintln!("  EMPIRICAL MIGRATION COST ESTIMATE (extrapolated from sample)");
    eprintln!("============================================================");
    eprintln!();
    eprintln!("Sample sent through surfpool:");
    eprintln!(
        "  transfers: {} entries in {} txs",
        transfer_agg.entries, transfer_agg.txs
    );
    eprintln!(
        "  accounts:  {} entries in {} txs",
        balance_agg.entries, balance_agg.txs
    );
    eprintln!();
    eprintln!("Per-tx averages (from sample):");
    eprintln!(
        "  BackfillNoReplay: fee={}L, cu={}, rent={}L per tx",
        transfer_agg.fee_lamports / transfer_agg.txs.max(1),
        transfer_agg.cu_consumed / transfer_agg.txs.max(1),
        transfer_agg.rent_lamports / transfer_agg.txs.max(1),
    );
    eprintln!(
        "  BackfillBalance:  fee={}L, cu={}, rent={}L per tx",
        balance_agg.fee_lamports / balance_agg.txs.max(1),
        balance_agg.cu_consumed / balance_agg.txs.max(1),
        balance_agg.rent_lamports / balance_agg.txs.max(1),
    );
    eprintln!();
    eprintln!("Full-catalogue extrapolation (linear by entry count):");
    eprintln!(
        "  BackfillNoReplay: {} entries / {} txs",
        FULL_TRANSFER_COUNT, tx_full_transfer
    );
    eprintln!("    fees:  {:>15} L  ({:>8.2} SOL  ${:>10.2})", t_fee, t_fee as f64 / LAMPORTS_PER_SOL, lamports_to_usd(t_fee));
    eprintln!("    rent:  {:>15} L  ({:>8.2} SOL  ${:>10.2})", t_rent, t_rent as f64 / LAMPORTS_PER_SOL, lamports_to_usd(t_rent));
    eprintln!(
        "  BackfillBalance:  {} entries / {} txs",
        FULL_ACCOUNT_COUNT, tx_full_balance
    );
    eprintln!("    fees:  {:>15} L  ({:>8.2} SOL  ${:>10.2})", a_fee, a_fee as f64 / LAMPORTS_PER_SOL, lamports_to_usd(a_fee));
    eprintln!("    rent:  {:>15} L  ({:>8.2} SOL  ${:>10.2})", a_rent, a_rent as f64 / LAMPORTS_PER_SOL, lamports_to_usd(a_rent));
    eprintln!();
    eprintln!("  Registrations + modifications: {} + {} = 46 txs via operational program (Shim CPI ~$0.50/tx worst case ⇒ ~$23).", FULL_REGISTRATION_COUNT, FULL_MODIFICATION_COUNT);
    eprintln!();
    eprintln!("TOTAL (sample-derived):");
    eprintln!(
        "  fees:      {:>15} L  ({:>8.2} SOL  ${:>10.2})  [burned]",
        total_fee,
        total_fee as f64 / LAMPORTS_PER_SOL,
        lamports_to_usd(total_fee)
    );
    eprintln!(
        "  rent:      {:>15} L  ({:>8.2} SOL  ${:>10.2})  [locked in PDAs]",
        total_rent,
        total_rent as f64 / LAMPORTS_PER_SOL,
        lamports_to_usd(total_rent)
    );
    eprintln!(
        "  GRAND:     {:>15} L  ({:>8.2} SOL  ${:>10.2})",
        grand,
        grand as f64 / LAMPORTS_PER_SOL,
        lamports_to_usd(grand)
    );
    eprintln!();
    eprintln!("Assumptions:");
    eprintln!("  - linear extrapolation from sample entries to full catalogue counts");
    eprintln!("  - no priority fees (surfpool default); mainnet may add $0-$3000 depending on congestion");
    eprintln!("  - SOL/USD = ${:.2} (informational)", SOL_USD);
    eprintln!("============================================================");
}
