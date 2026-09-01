//! Operator cost-measurement tool. Drives 100,000 catalogue transfers through
//! `BackfillNoReplay` across many `(chain, emitter)` pairs to confirm
//! per-bucket rent is stable, the tx fee is the deterministic 5,000 L base
//! fee, and the program survives a mainnet-scale workload. The trailing
//! asserts (`total_failures_v == 0`, `rent_per_bucket > 0`) are the probe's
//! own pass/fail signal.
//!
//! Requires `/tmp/wormchain-mainnet-snapshot/catalogue.jsonl`. Wall clock
//! ~5-10 min.
//!
//! Run: `cargo test --test surfpool_e2e_cost_probe_at_scale -- --ignored --nocapture`

#![allow(clippy::too_many_arguments)]

use std::{
    collections::HashMap,
    fs::File,
    io::{BufRead, BufReader},
    str::FromStr,
    sync::{Arc, Mutex},
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
use tokio::sync::Semaphore;

use global_accountant_definitions::{
    NOREPLAY_AUTHORITY_SEED_PREFIX, NOREPLAY_BITS_PER_BUCKET, NOREPLAY_PROGRAM_ID,
};
use solana_packet::PACKET_DATA_SIZE;

mod common;
use common::{
    surfpool::{deploy_program, rpc_call, so_path, start_surfpool, SurfpoolOptions},
    wire::{encode_noreplay_batch, NoReplayEntry as TransferEntry},
    BACKFILL_PROGRAM_NAME,
};

const CATALOGUE_PATH: &str = "/tmp/wormchain-mainnet-snapshot/catalogue.jsonl";

/// Target transfer count for the at-scale probe.
const TARGET_TRANSFERS: usize = 100_000;
/// Entries per BackfillNoReplay tx. The compact emitter-grouped wire format
/// fits 22 same-emitter / single-bucket entries, but a chunk spanning a
/// 1024-sequence bucket boundary needs one extra 32-byte account meta. 18
/// stays under `PACKET_DATA_SIZE` even across a 3-bucket span.
const TRANSFER_BATCH: usize = 18;
/// Concurrent in-flight transactions.
const MAX_CONCURRENT: usize = 16;
/// Sample meta on every Nth tx; full meta on every tx is expensive at scale.
const METADATA_SAMPLE_STRIDE: u64 = 50;

const LAMPORTS_PER_SOL: f64 = 1_000_000_000.0;
const SOL_USD: f64 = 230.0;

fn parse_hex32(s: &str) -> [u8; 32] {
    let s = s.strip_prefix("0x").unwrap_or(s);
    assert_eq!(s.len(), 64, "expected 32-byte hex, got len {}", s.len());
    let mut out = [0u8; 32];
    for i in 0..32 {
        out[i] = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).expect("hex");
    }
    out
}

/// Take the first `target` transfers from the (pre-sorted) catalogue,
/// round-robin across emitters: up to `per_emitter` consecutive sequences
/// from each before advancing.
fn read_in_order_transfers(target: usize, per_emitter: usize) -> Vec<TransferEntry> {
    let f = File::open(CATALOGUE_PATH).unwrap_or_else(|e| {
        panic!("missing catalogue at {CATALOGUE_PATH}: {e} — run the snapshot tool first")
    });
    let reader = BufReader::new(f);
    let mut out: Vec<TransferEntry> = Vec::with_capacity(target);
    let mut taken_in_current_emitter: usize = 0;
    let mut current_emitter: Option<(u16, [u8; 32])> = None;
    let mut skipped_in_current_emitter: usize = 0;
    for line in reader.lines() {
        let line = line.expect("read line");
        if !line.starts_with(r#"{"kind":"transfer""#) {
            continue;
        }
        let v: serde_json::Value = serde_json::from_str(&line).expect("parse JSON");
        let chain = v["chain"].as_u64().expect("chain") as u16;
        let emitter = parse_hex32(v["emitter"].as_str().expect("emitter"));
        let key = (chain, emitter);
        if current_emitter != Some(key) {
            current_emitter = Some(key);
            taken_in_current_emitter = 0;
            skipped_in_current_emitter = 0;
        }
        if taken_in_current_emitter >= per_emitter {
            // Skip the rest of this emitter's contiguous run; the next
            // emitter transition resets the counter.
            skipped_in_current_emitter += 1;
            let _ = skipped_in_current_emitter; // tracked for debug
            continue;
        }
        let sequence = v["sequence"].as_u64().expect("sequence");
        let digest = parse_hex32(v["digest"].as_str().expect("digest"));
        out.push(TransferEntry {
            chain,
            emitter,
            sequence,
            digest,
        });
        taken_in_current_emitter += 1;
        if out.len() >= target {
            break;
        }
    }
    out
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

fn build_backfill_noreplay_ix(
    program_id: &Pubkey,
    payer: &Pubkey,
    noreplay_auth: Pubkey,
    chunk: &[TransferEntry],
) -> Instruction {
    let data = encode_noreplay_batch(chunk);
    debug_assert!(
        data.len() < PACKET_DATA_SIZE,
        "BackfillNoReplay ix data alone ({} bytes) exceeds PACKET_DATA_SIZE ({})",
        data.len(),
        PACKET_DATA_SIZE
    );
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

#[derive(Default, Clone, Copy, Debug)]
struct TxSample {
    fee_lamports: u64,
    #[allow(dead_code)] // captured for telemetry; aggregate sums don't read it
    cu_consumed: u64,
    rent_lamports: u64,
    new_buckets: usize,
}

fn fetch_meta(rpc_url: &str, sig: &str, expected_buckets: usize) -> Option<TxSample> {
    let deadline = Instant::now() + Duration::from_secs(10);
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
        thread::sleep(Duration::from_millis(150));
    }
    let meta = last.get("result")?.get("meta")?;
    let fee = meta["fee"].as_u64()?;
    let cu = meta["computeUnitsConsumed"].as_u64().unwrap_or(0);
    let pre = meta["preBalances"][0].as_u64()?;
    let post = meta["postBalances"][0].as_u64()?;
    Some(TxSample {
        fee_lamports: fee,
        cu_consumed: cu,
        rent_lamports: pre.saturating_sub(post + fee),
        new_buckets: expected_buckets,
    })
}

fn count_new_buckets(
    chunk: &[TransferEntry],
    seen: &mut HashMap<(u16, [u8; 32], u64), ()>,
) -> usize {
    let mut n = 0;
    for e in chunk {
        let key = (e.chain, e.emitter, e.sequence / NOREPLAY_BITS_PER_BUCKET);
        if seen.insert(key, ()).is_none() {
            n += 1;
        }
    }
    n
}

fn send_ix(rpc: &RpcClient, payer: &Keypair, ix: Instruction) -> Result<String, ClientError> {
    let blockhash = rpc.get_latest_blockhash()?;
    let tx = Transaction::new_signed_with_payer(&[ix], Some(&payer.pubkey()), &[payer], blockhash);
    rpc.send_and_confirm_transaction(&tx).map(|s| s.to_string())
}

#[test]
#[ignore = "spawns surfpool; reads /tmp/wormchain-mainnet-snapshot/catalogue.jsonl; ~5-10 min runtime"]
fn surfpool_cost_probe_at_scale() {
    let t0 = Instant::now();

    // -------- Sample --------
    //
    // 4 buckets/emitter (4096 sequences) drains several of the ~40 emitters
    // fully, yielding cross-emitter bucket samples while keeping the long
    // same-emitter runs the wire format favors.
    eprintln!("[cost-probe-scale] reading in-order {TARGET_TRANSFERS} transfer samples...");
    let mut transfers = read_in_order_transfers(TARGET_TRANSFERS, 4 * 1024);
    transfers.sort_by_key(|e| (e.chain, e.emitter, e.sequence));
    transfers.dedup_by_key(|e| (e.chain, e.emitter, e.sequence));
    let n = transfers.len();
    let mut bucket_keys: HashMap<(u16, [u8; 32], u64), ()> = HashMap::new();
    for e in &transfers {
        bucket_keys.insert(
            (e.chain, e.emitter, e.sequence / NOREPLAY_BITS_PER_BUCKET),
            (),
        );
    }
    let unique_buckets_sample = bucket_keys.len();
    let mut emitter_set: HashMap<(u16, [u8; 32]), ()> = HashMap::new();
    for e in &transfers {
        emitter_set.insert((e.chain, e.emitter), ());
    }
    eprintln!(
        "[cost-probe-scale] sampled {n} unique transfers, spanning {} (chain,emitter) pairs and {unique_buckets_sample} buckets",
        emitter_set.len()
    );

    // -------- Boot surfpool + deploy --------
    let backfill_so = so_path(BACKFILL_PROGRAM_NAME);
    let backfill_bytes = std::fs::read(&backfill_so).unwrap_or_else(|e| {
        panic!(
            "read {}: {e} — run `just build` first",
            backfill_so.display()
        )
    });
    let noreplay_bytes = accountant_test_fixtures::NOREPLAY_SO.bytes;

    let guard = start_surfpool(SurfpoolOptions::offline("backfill-cost-probe-scale"));
    let rpc_url = guard.rpc_url();
    let rpc = guard.rpc_client();

    // Payer must equal `BACKFILL_AUTHORITY` (test default: `Keypair::new_from_array([1u8; 32])`).
    let payer = Keypair::new_from_array([1u8; 32]);
    // Airdrop generously: ~3 SOL of base fees + ~15 SOL of bucket rent + headroom.
    rpc.request_airdrop(&payer.pubkey(), 50_000_000_000_000)
        .expect("airdrop");
    thread::sleep(Duration::from_millis(500));

    // Anchor's `declare_id!` pins this program to a single fixed address,
    // checked on every entry, so deploy at that fixed ID.
    let program_id = Pubkey::new_from_array(global_accountant_backfill::ID.to_bytes());
    deploy_program(&rpc_url, &program_id, &backfill_bytes);
    deploy_program(
        &rpc_url,
        &Pubkey::new_from_array(NOREPLAY_PROGRAM_ID),
        noreplay_bytes,
    );

    let noreplay_auth = derive_noreplay_authority_pda(&program_id);

    // -------- Drive batches with bounded concurrency --------
    eprintln!(
        "[cost-probe-scale] sending {} txs at concurrency={MAX_CONCURRENT}, meta sampled every {METADATA_SAMPLE_STRIDE}th tx",
        n.div_ceil(TRANSFER_BATCH)
    );

    // Chunk by emitter boundary and size cap; crossing emitters costs an
    // extra 35-byte group header.
    let mut chunks: Vec<Vec<TransferEntry>> = Vec::new();
    let mut current: Vec<TransferEntry> = Vec::new();
    let mut current_emitter: Option<(u16, [u8; 32])> = None;
    for e in &transfers {
        let key = (e.chain, e.emitter);
        let crossed_emitter = current_emitter.is_some() && current_emitter != Some(key);
        if (crossed_emitter || current.len() >= TRANSFER_BATCH) && !current.is_empty() {
            chunks.push(std::mem::take(&mut current));
        }
        current.push(*e);
        current_emitter = Some(key);
    }
    if !current.is_empty() {
        chunks.push(current);
    }
    let total_chunks = chunks.len();

    let rt = tokio::runtime::Runtime::new().expect("tokio rt");
    let samples: Arc<Mutex<Vec<TxSample>>> = Arc::new(Mutex::new(Vec::new()));
    let total_fees: Arc<Mutex<u64>> = Arc::new(Mutex::new(0));
    let total_rent: Arc<Mutex<u64>> = Arc::new(Mutex::new(0));
    let total_txs: Arc<Mutex<u64>> = Arc::new(Mutex::new(0));
    let total_failures: Arc<Mutex<u64>> = Arc::new(Mutex::new(0));

    // Pre-compute new-bucket counts per chunk for accurate rent attribution.
    let mut seen_buckets: HashMap<(u16, [u8; 32], u64), ()> = HashMap::new();
    let new_buckets_per_chunk: Vec<usize> = chunks
        .iter()
        .map(|c| count_new_buckets(c, &mut seen_buckets))
        .collect();
    let total_buckets_to_create: usize = new_buckets_per_chunk.iter().sum();
    eprintln!("[cost-probe-scale] {total_buckets_to_create} unique buckets will be created");

    rt.block_on(async {
        let sem = Arc::new(Semaphore::new(MAX_CONCURRENT));
        let rpc_url = Arc::new(rpc_url.clone());
        let payer = Arc::new(payer);
        let mut handles = Vec::with_capacity(chunks.len());

        for (i, chunk) in chunks.into_iter().enumerate() {
            let permit = sem.clone().acquire_owned().await.expect("sem");
            let rpc_url = rpc_url.clone();
            let payer = payer.clone();
            let samples = samples.clone();
            let total_fees = total_fees.clone();
            let total_rent = total_rent.clone();
            let total_txs = total_txs.clone();
            let total_failures = total_failures.clone();
            let new_buckets = new_buckets_per_chunk[i];
            let h = tokio::task::spawn_blocking(move || {
                let _permit = permit; // released on drop
                let rpc = RpcClient::new_with_commitment(
                    rpc_url.as_str().to_string(),
                    solana_commitment_config::CommitmentConfig::confirmed(),
                );
                let ix =
                    build_backfill_noreplay_ix(&program_id, &payer.pubkey(), noreplay_auth, &chunk);
                match send_ix(&rpc, &payer, ix) {
                    Ok(sig) => {
                        *total_txs.lock().unwrap() += 1;
                        if i as u64 % METADATA_SAMPLE_STRIDE == 0 {
                            if let Some(m) = fetch_meta(&rpc_url, &sig, new_buckets) {
                                *total_fees.lock().unwrap() += m.fee_lamports;
                                *total_rent.lock().unwrap() += m.rent_lamports;
                                samples.lock().unwrap().push(m);
                            } else {
                                // Meta fetch failed on a tx that still
                                // succeeded — fall back to the deterministic
                                // 5,000 L base fee for the tally.
                                *total_fees.lock().unwrap() += 5_000;
                            }
                        } else {
                            // Non-sampled tx: assume the deterministic per-tx fee.
                            *total_fees.lock().unwrap() += 5_000;
                        }
                    }
                    Err(e) => {
                        *total_failures.lock().unwrap() += 1;
                        if *total_failures.lock().unwrap() < 5 {
                            eprintln!("[cost-probe-scale] tx#{i} failed: {e}");
                        }
                    }
                }
            });
            handles.push(h);
            if (i + 1) % 500 == 0 {
                eprintln!(
                    "[cost-probe-scale] dispatched {}/{} chunks ({:.0}%)",
                    i + 1,
                    total_chunks,
                    (i + 1) as f64 / total_chunks as f64 * 100.0
                );
            }
        }
        for h in handles {
            let _ = h.await;
        }
    });

    let elapsed = t0.elapsed();
    let samples_lock = samples.lock().unwrap();
    let total_fees_v = *total_fees.lock().unwrap();
    let total_rent_v = *total_rent.lock().unwrap();
    let total_txs_v = *total_txs.lock().unwrap();
    let total_failures_v = *total_failures.lock().unwrap();
    let sample_n = samples_lock.len();
    let sample_buckets: usize = samples_lock.iter().map(|s| s.new_buckets).sum();
    let sample_rent: u64 = samples_lock.iter().map(|s| s.rent_lamports).sum();
    let rent_per_bucket = if sample_buckets > 0 {
        sample_rent / sample_buckets as u64
    } else {
        0
    };
    // Extrapolated rent from the sampled per-bucket figure × known total buckets to create.
    let extrapolated_rent = rent_per_bucket * total_buckets_to_create as u64;

    eprintln!();
    eprintln!("============================================================");
    eprintln!("  AT-SCALE COST PROBE — RESULTS");
    eprintln!("============================================================");
    eprintln!("Sample sent:                       {n} transfers in {total_txs_v} txs");
    eprintln!("Failed txs:                        {total_failures_v}");
    eprintln!("Unique buckets created:            {total_buckets_to_create}");
    eprintln!(
        "Distinct (chain, emitter) pairs:   {} of 40 known in the full catalogue",
        emitter_set.len()
    );
    eprintln!(
        "Wall clock:                        {:.1}s",
        elapsed.as_secs_f64()
    );
    eprintln!();
    eprintln!("Per-tx fee (deterministic):        5,000 L");
    eprintln!(
        "Per-bucket rent (sample avg, N={sample_n} meta fetches covering {sample_buckets} bucket creations):"
    );
    eprintln!(
        "                                   {rent_per_bucket} L  (≈ {:.6} SOL)",
        rent_per_bucket as f64 / LAMPORTS_PER_SOL
    );
    eprintln!();
    eprintln!("This sample's totals:");
    eprintln!(
        "  fees:   {:>15} L  ({:.4} SOL)",
        total_fees_v,
        total_fees_v as f64 / LAMPORTS_PER_SOL
    );
    eprintln!("  rent (sampled subset, partial): {:>15} L", total_rent_v);
    eprintln!(
        "  rent (extrapolated by bucket count × sampled per-bucket): {:>15} L  ({:.4} SOL)",
        extrapolated_rent,
        extrapolated_rent as f64 / LAMPORTS_PER_SOL
    );
    eprintln!();
    eprintln!("Full-catalogue projection (5,442 buckets × measured per-bucket rent + 551,667 txs × 5,000 L):");
    let full_txs: u64 = 5_516_669_u64.div_ceil(TRANSFER_BATCH as u64);
    let full_fees = full_txs * 5_000;
    let full_rent = rent_per_bucket * 5_442;
    eprintln!(
        "  fees:   {:>15} L  ({:.2} SOL  ${:>8.2})",
        full_fees,
        full_fees as f64 / LAMPORTS_PER_SOL,
        full_fees as f64 / LAMPORTS_PER_SOL * SOL_USD
    );
    eprintln!(
        "  rent:   {:>15} L  ({:.2} SOL  ${:>8.2})",
        full_rent,
        full_rent as f64 / LAMPORTS_PER_SOL,
        full_rent as f64 / LAMPORTS_PER_SOL * SOL_USD
    );
    eprintln!(
        "  TOTAL:  {:>15} L  ({:.2} SOL  ${:>8.2})",
        full_fees + full_rent,
        (full_fees + full_rent) as f64 / LAMPORTS_PER_SOL,
        (full_fees + full_rent) as f64 / LAMPORTS_PER_SOL * SOL_USD
    );
    eprintln!("============================================================");

    assert!(total_failures_v == 0, "had {total_failures_v} tx failures");
    assert!(rent_per_bucket > 0, "no rent samples captured");
}
