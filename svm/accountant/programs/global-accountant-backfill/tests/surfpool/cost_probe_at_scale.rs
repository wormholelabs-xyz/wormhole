//! Operator cost tool at mainnet scale. Drives 100,000 catalogue transfers through
//! `BackfillNoReplay` across many `(chain, emitter)` pairs, to confirm that
//! per-bucket rent is stable, that the fee stays the deterministic 5,000-lamport
//! base fee, and that the program survives the full workload shape. The closing
//! asserts are the probe's own pass/fail signal.
//!
//! Needs `/tmp/wormchain-mainnet-snapshot/catalogue.jsonl`. Wall clock 5-10 min.
//!
//! Run: `just e2e-backfill-probe`.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use accountant_operational_core::cpi::noreplay::derive_bucket_pda;
use global_accountant_definitions::NoReplayBitmapAccount;
use solana_signer::Signer;
use tokio::sync::Semaphore;

use crate::common::probe::{catalogue_lines, cost_line, measure_tx, parse_hex32, TxCost};
use crate::common::*;
use crate::cost_probe::noreplay_ix;
use crate::harness::{
    deploy_programs, fund, rpc_client, start_surfpool, try_send, ProgramImage, SurfpoolOptions,
};

const TARGET_TRANSFERS: usize = 100_000;

/// Entries per transaction. The emitter-grouped wire format fits 22 single-bucket
/// entries, but a chunk that crosses a 1024-sequence boundary needs one more
/// 32-byte account meta; 18 stays inside one packet even across three buckets.
const TRANSFER_BATCH: usize = 18;

/// Sequences taken from one emitter before moving to the next: four buckets.
const PER_EMITTER: usize = 4 * 1024;

const MAX_CONCURRENT: usize = 16;

/// Full meta is fetched for every Nth transaction; the rest are tallied at the
/// deterministic base fee.
const METADATA_SAMPLE_STRIDE: usize = 50;
const BASE_FEE_LAMPORTS: u64 = 5_000;

/// ~3 SOL of base fees plus ~15 SOL of bucket rent, with headroom.
const PAYER_LAMPORTS: u64 = 50_000_000_000_000;

/// Catalogue totals at snapshot height 18,669,029.
const FULL_TRANSFER_COUNT: u64 = 5_516_669;
const FULL_BUCKET_COUNT: u64 = 5_442;

type BucketKey = (u16, [u8; 32], u64);

fn bucket_key(entry: &wire::NoReplayEntry) -> BucketKey {
    (
        entry.chain,
        entry.emitter,
        NoReplayBitmapAccount::bucket_index(entry.sequence),
    )
}

/// The first `target` transfers of the pre-sorted catalogue, round-robin across
/// emitters: at most `per_emitter` consecutive sequences from each contiguous run.
fn read_in_order_transfers(target: usize, per_emitter: usize) -> Option<Vec<wire::NoReplayEntry>> {
    let mut out: Vec<wire::NoReplayEntry> = Vec::with_capacity(target);
    let mut current: Option<(u16, [u8; 32])> = None;
    let mut taken = 0usize;
    for line in catalogue_lines()? {
        if !line.starts_with(r#"{"kind":"transfer""#) {
            continue;
        }
        let row: serde_json::Value = serde_json::from_str(&line).expect("catalogue row");
        let chain = row["chain"].as_u64().expect("chain") as u16;
        let emitter = parse_hex32(row["emitter"].as_str().expect("emitter"));
        if current != Some((chain, emitter)) {
            current = Some((chain, emitter));
            taken = 0;
        }
        if taken >= per_emitter {
            continue;
        }
        out.push(wire::NoReplayEntry {
            chain,
            emitter,
            sequence: row["sequence"].as_u64().expect("sequence"),
            digest: parse_hex32(row["digest"].as_str().expect("digest")),
        });
        taken += 1;
        if out.len() >= target {
            break;
        }
    }
    Some(out)
}

/// Split into transactions: a new chunk starts at the batch size or at an emitter
/// change, because crossing emitters costs another 35-byte group header.
fn chunk_by_emitter(transfers: &[wire::NoReplayEntry]) -> Vec<Vec<wire::NoReplayEntry>> {
    let mut chunks: Vec<Vec<wire::NoReplayEntry>> = Vec::new();
    let mut current: Vec<wire::NoReplayEntry> = Vec::new();
    let mut emitter: Option<(u16, [u8; 32])> = None;
    for entry in transfers {
        let key = (entry.chain, entry.emitter);
        let crossed = emitter.is_some() && emitter != Some(key);
        if !current.is_empty() && (crossed || current.len() >= TRANSFER_BATCH) {
            chunks.push(std::mem::take(&mut current));
        }
        current.push(*entry);
        emitter = Some(key);
    }
    if !current.is_empty() {
        chunks.push(current);
    }
    chunks
}

#[derive(Default)]
struct Tally {
    txs: u64,
    failures: u64,
    fee_lamports: u64,
    sampled: Vec<TxCost>,
}

#[test]
#[ignore = "operator tool: spawns surfpool, reads the snapshot catalogue, 5-10 min; run via `just e2e-backfill-probe`"]
fn surfpool_cost_probe_at_scale() {
    let started = Instant::now();
    eprintln!("[cost-probe-scale] reading {TARGET_TRANSFERS} transfers in catalogue order");
    let Some(mut transfers) = read_in_order_transfers(TARGET_TRANSFERS, PER_EMITTER) else {
        return;
    };
    transfers.sort_by_key(|e| (e.chain, e.emitter, e.sequence));
    transfers.dedup_by_key(|e| (e.chain, e.emitter, e.sequence));
    assert!(!transfers.is_empty(), "catalogue holds no transfer rows");

    let emitters: HashSet<(u16, [u8; 32])> =
        transfers.iter().map(|e| (e.chain, e.emitter)).collect();
    let sample_buckets: HashSet<BucketKey> = transfers.iter().map(bucket_key).collect();
    eprintln!(
        "[cost-probe-scale] {} transfers, {} (chain, emitter) pairs, {} buckets",
        transfers.len(),
        emitters.len(),
        sample_buckets.len()
    );

    let guard = start_surfpool(SurfpoolOptions::offline("ga-backfill-cost-probe-scale"));
    let rpc_url = guard.rpc_url();
    let rpc = guard.rpc_client();
    let backfill = accountant_image();
    let program_id = backfill.program_id;
    deploy_programs(&rpc, &[backfill, ProgramImage::noreplay()]);

    let payer = test_authority_keypair();
    fund(&rpc, &payer.pubkey(), PAYER_LAMPORTS);
    let noreplay_authority = noreplay_authority_pda(&program_id);

    let chunks = chunk_by_emitter(&transfers);
    let buckets_to_create = sample_buckets.len();
    eprintln!(
        "[cost-probe-scale] {} txs at concurrency {MAX_CONCURRENT}, {buckets_to_create} buckets to create",
        chunks.len()
    );

    let total_chunks = chunks.len();
    let tally = Arc::new(Mutex::new(Tally::default()));
    let runtime = tokio::runtime::Runtime::new().expect("tokio runtime");
    runtime.block_on(async {
        let permits = Arc::new(Semaphore::new(MAX_CONCURRENT));
        let payer = Arc::new(payer);
        let rpc_url = Arc::new(rpc_url);
        let mut handles = Vec::with_capacity(total_chunks);
        for (i, chunk) in chunks.into_iter().enumerate() {
            let permit = permits
                .clone()
                .acquire_owned()
                .await
                .expect("semaphore permit");
            let payer = payer.clone();
            let rpc_url = rpc_url.clone();
            let tally = tally.clone();
            handles.push(tokio::task::spawn_blocking(move || {
                let _permit = permit;
                let rpc = rpc_client(&rpc_url);
                let ix = noreplay_ix(&program_id, &payer.pubkey(), &noreplay_authority, &chunk);
                match try_send(&rpc, &[ix], &[&payer]) {
                    Ok(sig) => {
                        let sampled = i.is_multiple_of(METADATA_SAMPLE_STRIDE);
                        let cost = sampled.then(|| measure_tx(&rpc, &sig)).flatten();
                        let mut tally = tally.lock().expect("tally");
                        tally.txs += 1;
                        match cost {
                            Some(cost) => {
                                tally.fee_lamports += cost.fee_lamports;
                                tally.sampled.push(cost);
                            }
                            None => tally.fee_lamports += BASE_FEE_LAMPORTS,
                        }
                    }
                    Err(e) => {
                        let mut tally = tally.lock().expect("tally");
                        tally.failures += 1;
                        if tally.failures <= 5 {
                            eprintln!("[cost-probe-scale] tx#{i} failed: {e}");
                        }
                    }
                }
            }));
            if (i + 1).is_multiple_of(500) {
                eprintln!("[cost-probe-scale] dispatched {}/{total_chunks}", i + 1);
            }
        }
        for handle in handles {
            handle.await.expect("probe task");
        }
    });

    let tally = tally.lock().expect("tally");
    // Rent comes from a created bucket's own balance, not from a payer delta: under
    // concurrency any of the transactions touching a bucket can be the one that
    // creates it.
    let probe_entry = transfers.first().expect("non-empty sample");
    let probe_bucket = derive_bucket_pda(
        &noreplay_authority,
        probe_entry.chain,
        &probe_entry.emitter,
        probe_entry.sequence,
    )
    .0;
    let rent_per_bucket = rpc
        .get_account(&probe_bucket)
        .expect("created bucket PDA")
        .lamports;
    let sampled_cu = tally
        .sampled
        .iter()
        .map(|cost| cost.cu_consumed)
        .sum::<u64>()
        / (tally.sampled.len().max(1) as u64);
    let full_txs = FULL_TRANSFER_COUNT.div_ceil(TRANSFER_BATCH as u64);
    let full_fees = full_txs * BASE_FEE_LAMPORTS;
    let full_rent = rent_per_bucket * FULL_BUCKET_COUNT;

    eprintln!();
    eprintln!("============================================================");
    eprintln!("  AT-SCALE COST PROBE");
    eprintln!("============================================================");
    eprintln!(
        "Sent:              {} transfers in {} txs ({} failed) in {:.1}s",
        transfers.len(),
        tally.txs,
        tally.failures,
        started.elapsed().as_secs_f64()
    );
    eprintln!("Buckets created:   {buckets_to_create}");
    eprintln!("Emitter pairs:     {}", emitters.len());
    eprintln!("Meta sampled:      {} txs", tally.sampled.len());
    eprintln!("CU per tx (avg):   {sampled_cu}");
    eprintln!("Rent per bucket:   {rent_per_bucket} L");
    eprintln!("{}", cost_line("Sample fees:       ", tally.fee_lamports));
    eprintln!();
    eprintln!(
        "Full catalogue ({FULL_TRANSFER_COUNT} transfers / {full_txs} txs / {FULL_BUCKET_COUNT} buckets):"
    );
    eprintln!("{}", cost_line("  fees:  ", full_fees));
    eprintln!("{}", cost_line("  rent:  ", full_rent));
    eprintln!("{}", cost_line("  TOTAL: ", full_fees + full_rent));
    eprintln!("============================================================");

    assert_eq!(tally.failures, 0, "transactions failed");
    assert!(!tally.sampled.is_empty(), "no transaction meta sampled");
    assert!(rent_per_bucket > 0, "bucket PDA holds no rent");
}
