//! Operator cost tool. Sends a small sample of
//! `BackfillNoReplay` and `BackfillBalance` entries from the wormchain snapshot
//! catalogue through a fresh surfpool, measures fee, compute units and rent per
//! transaction, and extrapolates linearly to the full catalogue.
//!
//! Needs `/tmp/wormchain-mainnet-snapshot/catalogue.jsonl`, staged by the snapshot
//! tool; the probe prints a skip message and returns when it is absent. The
//! `cost_probe_at_scale` sibling runs the same workload two orders of magnitude
//! larger.
//!
//! Run: `just e2e-backfill-probe`.

use accountant_operational_core::accounts::balance;
use accountant_operational_core::cpi::noreplay::derive_bucket_pda;
use global_accountant_definitions::global_accountant_backfill::Instruction as Arm;
use global_accountant_definitions::{
    BackfillBalanceEntry, NoReplayBitmapAccount, NOREPLAY_PROGRAM_ID,
};
use solana_instruction::{AccountMeta, Instruction};
use solana_packet::PACKET_DATA_SIZE;
use solana_pubkey::Pubkey;
use solana_signer::Signer;

use crate::common::probe::{
    catalogue_lines, cost_line, lamports_to_usd, measure_tx, parse_hex32, Aggregate,
    LAMPORTS_PER_SOL, SOL_USD,
};
use crate::common::*;
use crate::harness::{deploy_programs, fund, send, start_surfpool, ProgramImage, SurfpoolOptions};

/// Catalogue totals at snapshot height 18,669,029.
const FULL_TRANSFER_COUNT: u64 = 5_516_669;
const FULL_ACCOUNT_COUNT: u64 = 17_367;
const FULL_REGISTRATION_COUNT: u64 = 40;
const FULL_MODIFICATION_COUNT: u64 = 6;

/// Sample sizes: small enough to finish in seconds, large enough that per-tx
/// variance averages out.
const TRANSFER_SAMPLE: usize = 100;
const ACCOUNT_SAMPLE: usize = 50;

/// Entries per transaction. Both sit below `PACKET_DATA_SIZE` once the account metas and the
/// instruction data are added up; the builders below assert the data half.
const TRANSFER_BATCH: usize = 10;
const ACCOUNT_BATCH: usize = 8;

const PAYER_LAMPORTS: u64 = 1_000_000_000_000;

/// Unique `(chain, emitter, bucket)` PDAs in walk order. The handler consumes the
/// bucket slots in this same order, flushing on each transition.
pub(crate) fn bucket_metas(
    noreplay_authority: &Pubkey,
    entries: &[wire::NoReplayEntry],
) -> Vec<AccountMeta> {
    let mut metas = Vec::new();
    let mut previous: Option<(u16, [u8; 32], u64)> = None;
    for entry in entries {
        let key = (
            entry.chain,
            entry.emitter,
            NoReplayBitmapAccount::bucket_index(entry.sequence),
        );
        if previous != Some(key) {
            let pda = derive_bucket_pda(
                noreplay_authority,
                entry.chain,
                &entry.emitter,
                entry.sequence,
            )
            .0;
            metas.push(AccountMeta::new(pda, false));
            previous = Some(key);
        }
    }
    metas
}

pub(crate) fn noreplay_ix(
    program_id: &Pubkey,
    payer: &Pubkey,
    noreplay_authority: &Pubkey,
    entries: &[wire::NoReplayEntry],
) -> Instruction {
    let mut accounts = vec![
        AccountMeta::new(*payer, true),
        AccountMeta::new_readonly(Pubkey::new_from_array(NOREPLAY_PROGRAM_ID), false),
        AccountMeta::new_readonly(*noreplay_authority, false),
        AccountMeta::new_readonly(system_program_id(), false),
    ];
    accounts.extend(bucket_metas(noreplay_authority, entries));
    let data = wire::encode_noreplay_batch(Arm::BackfillNoReplay as u8, entries);
    debug_assert!(
        data.len() < PACKET_DATA_SIZE,
        "BackfillNoReplay ix data alone ({} bytes) exceeds PACKET_DATA_SIZE ({})",
        data.len(),
        PACKET_DATA_SIZE
    );
    Instruction {
        program_id: *program_id,
        accounts,
        data,
    }
}

fn balance_ix(
    program_id: &Pubkey,
    payer: &Pubkey,
    entries: &[BackfillBalanceEntry],
) -> Instruction {
    let mut accounts = vec![
        AccountMeta::new(*payer, true),
        AccountMeta::new_readonly(system_program_id(), false),
    ];
    accounts.extend(entries.iter().map(|entry| {
        let pda = balance::derive_pda(
            program_id,
            entry.chain(),
            entry.token_chain(),
            &entry.token_address,
        )
        .0;
        AccountMeta::new(pda, false)
    }));
    let data = wire::encode_balance_batch(Arm::BackfillBalance as u8, entries);
    debug_assert!(
        data.len() < PACKET_DATA_SIZE,
        "BackfillBalance ix data alone ({} bytes) exceeds PACKET_DATA_SIZE ({})",
        data.len(),
        PACKET_DATA_SIZE
    );
    Instruction {
        program_id: *program_id,
        accounts,
        data,
    }
}

/// First `TRANSFER_SAMPLE` transfer rows and `ACCOUNT_SAMPLE` account rows of the
/// catalogue.
fn read_samples() -> Option<(Vec<wire::NoReplayEntry>, Vec<BackfillBalanceEntry>)> {
    let mut transfers: Vec<wire::NoReplayEntry> = Vec::with_capacity(TRANSFER_SAMPLE);
    let mut accounts: Vec<BackfillBalanceEntry> = Vec::with_capacity(ACCOUNT_SAMPLE);
    for line in catalogue_lines()? {
        if line.is_empty() {
            continue;
        }
        let row: serde_json::Value = serde_json::from_str(&line).expect("catalogue row");
        match row.get("kind").and_then(|k| k.as_str()).unwrap_or("") {
            "transfer" if transfers.len() < TRANSFER_SAMPLE => {
                transfers.push(wire::NoReplayEntry {
                    chain: row["chain"].as_u64().expect("chain") as u16,
                    emitter: parse_hex32(row["emitter"].as_str().expect("emitter")),
                    sequence: row["sequence"].as_u64().expect("sequence"),
                    digest: parse_hex32(row["digest"].as_str().expect("digest")),
                });
            }
            "account" if accounts.len() < ACCOUNT_SAMPLE => {
                accounts.push(wire::balance_entry(
                    row["chain"].as_u64().expect("chain") as u16,
                    row["token_chain"].as_u64().expect("token_chain") as u16,
                    parse_hex32(row["token_address"].as_str().expect("token_address")),
                    parse_hex32(row["balance"].as_str().expect("balance")),
                ));
            }
            _ => {}
        }
        if transfers.len() >= TRANSFER_SAMPLE && accounts.len() >= ACCOUNT_SAMPLE {
            break;
        }
    }
    assert!(!transfers.is_empty(), "catalogue holds no transfer rows");
    assert!(!accounts.is_empty(), "catalogue holds no account rows");
    Some((transfers, accounts))
}

#[test]
#[ignore = "operator tool: spawns surfpool and reads the snapshot catalogue; run via `just e2e-backfill-probe`"]
fn surfpool_cost_probe() {
    let Some((mut transfers, mut accounts)) = read_samples() else {
        return;
    };
    eprintln!(
        "[cost-probe] {} transfer + {} account samples",
        transfers.len(),
        accounts.len()
    );

    // Both wire formats demand strictly ascending keys.
    transfers.sort_by_key(|e| (e.chain, e.emitter, e.sequence));
    transfers.dedup_by_key(|e| (e.chain, e.emitter, e.sequence));
    accounts.sort_by_key(|e| e.sort_key());
    accounts.dedup_by_key(|e| e.sort_key());

    let guard = start_surfpool(SurfpoolOptions::offline("ga-backfill-cost-probe"));
    let rpc = guard.rpc_client();
    let backfill = accountant_image();
    let id = backfill.program_id;
    deploy_programs(&rpc, &[backfill, ProgramImage::noreplay()]);

    let payer = test_authority_keypair();
    fund(&rpc, &payer.pubkey(), PAYER_LAMPORTS);
    let noreplay_authority = noreplay_authority_pda(&id);

    let mut transfer_agg = Aggregate::default();
    for chunk in transfers.chunks(TRANSFER_BATCH) {
        let sig = send(
            &rpc,
            "backfill_no_replay",
            &[noreplay_ix(
                &id,
                &payer.pubkey(),
                &noreplay_authority,
                chunk,
            )],
            &[&payer],
        );
        let mut cost = measure_tx(&rpc, &sig).expect("BackfillNoReplay meta");
        cost.entry_count = chunk.len();
        eprintln!(
            "[cost-probe] NoReplay entries={} fee={}L cu={} rent={}L",
            cost.entry_count, cost.fee_lamports, cost.cu_consumed, cost.rent_lamports
        );
        transfer_agg.add(cost);
    }

    let mut balance_agg = Aggregate::default();
    for chunk in accounts.chunks(ACCOUNT_BATCH) {
        let sig = send(
            &rpc,
            "backfill_balance",
            &[balance_ix(&id, &payer.pubkey(), chunk)],
            &[&payer],
        );
        let mut cost = measure_tx(&rpc, &sig).expect("BackfillBalance meta");
        cost.entry_count = chunk.len();
        eprintln!(
            "[cost-probe] Balance  entries={} fee={}L cu={} rent={}L",
            cost.entry_count, cost.fee_lamports, cost.cu_consumed, cost.rent_lamports
        );
        balance_agg.add(cost);
    }

    let transfer_scale = transfer_agg.scale_to(FULL_TRANSFER_COUNT);
    let balance_scale = balance_agg.scale_to(FULL_ACCOUNT_COUNT);
    let transfer_txs = (transfer_agg.txs as f64 * transfer_scale).ceil() as u64;
    let balance_txs = (balance_agg.txs as f64 * balance_scale).ceil() as u64;
    let transfer_fee = (transfer_agg.fee_lamports as f64 * transfer_scale) as u64;
    let transfer_rent = (transfer_agg.rent_lamports as f64 * transfer_scale) as u64;
    let balance_fee = (balance_agg.fee_lamports as f64 * balance_scale) as u64;
    let balance_rent = (balance_agg.rent_lamports as f64 * balance_scale) as u64;
    let total_fee = transfer_fee + balance_fee;
    let total_rent = transfer_rent + balance_rent;

    eprintln!();
    eprintln!("============================================================");
    eprintln!("  MIGRATION COST ESTIMATE, extrapolated from the sample");
    eprintln!("============================================================");
    eprintln!(
        "Sample: {} transfers in {} txs, {} accounts in {} txs",
        transfer_agg.entries, transfer_agg.txs, balance_agg.entries, balance_agg.txs
    );
    eprintln!(
        "Per tx: BackfillNoReplay fee={}L cu={} rent={}L",
        transfer_agg.per_tx(transfer_agg.fee_lamports),
        transfer_agg.per_tx(transfer_agg.cu_consumed),
        transfer_agg.per_tx(transfer_agg.rent_lamports),
    );
    eprintln!(
        "Per tx: BackfillBalance  fee={}L cu={} rent={}L",
        balance_agg.per_tx(balance_agg.fee_lamports),
        balance_agg.per_tx(balance_agg.cu_consumed),
        balance_agg.per_tx(balance_agg.rent_lamports),
    );
    eprintln!();
    eprintln!("BackfillNoReplay: {FULL_TRANSFER_COUNT} entries / {transfer_txs} txs");
    eprintln!("{}", cost_line("  fees:  ", transfer_fee));
    eprintln!("{}", cost_line("  rent:  ", transfer_rent));
    eprintln!("BackfillBalance:  {FULL_ACCOUNT_COUNT} entries / {balance_txs} txs");
    eprintln!("{}", cost_line("  fees:  ", balance_fee));
    eprintln!("{}", cost_line("  rent:  ", balance_rent));
    eprintln!(
        "BackfillChainRegistration {FULL_REGISTRATION_COUNT} + BackfillModifyBalance \
         {FULL_MODIFICATION_COUNT} entries: one tx each."
    );
    eprintln!();
    eprintln!("{}", cost_line("TOTAL fees (burned):     ", total_fee));
    eprintln!("{}", cost_line("TOTAL rent (locked):     ", total_rent));
    eprintln!(
        "{}",
        cost_line("GRAND TOTAL:             ", total_fee + total_rent)
    );
    eprintln!();
    eprintln!("Linear extrapolation from the sample, at surfpool's zero priority fee.");
    eprintln!("Mainnet congestion adds a priority fee on top.");
    eprintln!("SOL/USD = {SOL_USD:.2}; 1 SOL = {LAMPORTS_PER_SOL:.0} lamports.");
    eprintln!(
        "One extra SOL of fees is ${:.2}.",
        lamports_to_usd(1_000_000_000)
    );
    eprintln!("============================================================");

    assert!(transfer_agg.txs > 0, "no BackfillNoReplay tx measured");
    assert!(balance_agg.txs > 0, "no BackfillBalance tx measured");
    assert!(
        transfer_agg.rent_lamports > 0,
        "BackfillNoReplay paid no bucket rent"
    );
    assert!(
        balance_agg.rent_lamports > 0,
        "BackfillBalance paid no PDA rent"
    );
}
