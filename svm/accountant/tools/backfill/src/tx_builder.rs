//! Build `solana_instruction::Instruction` values from batch plans.
//!
//! Entries are encoded through the definitions entry types
//! (`global_accountant_definitions::backfill_ix_data`), the types the program
//! parses with. [`build_ix`] then parses the data back with the program's own
//! batch parser and measures the exact legacy wire size, so a malformed batch
//! fails here, before any transaction is sent.

use anyhow::{anyhow, ensure, Context, Result};
use bytemuck::Pod;
use sha2::{Digest, Sha256};
use solana_instruction::{AccountMeta, Instruction};
use solana_pubkey::Pubkey;

use global_accountant_definitions::{
    BackfillBalanceEntry, BackfillChainRegistrationEntry, BackfillModifyBalanceEntry,
    BackfillNoReplayEntry, BackfillNoReplayGroupHeader, BackfillTransceiverHubEntry,
    BackfillTransceiverPeerEntry, BalanceAccountKey, BalanceBatch, ChainRegistrationBatch,
    ChainRegistrationKey, GlobalAccountantError, ModifyBalanceBatch, ModifyBalanceKey,
    NoReplayBatch, RegisterChainKey, TransceiverHubBatch, TransceiverHubKey, TransceiverPeerBatch,
    TransceiverPeerKey,
};

use svm_accountant_tools_common::ids::BucketId;
use svm_accountant_tools_common::layouts::derive_pda;
use svm_accountant_tools_common::noreplay::{bucket_id, derive_bucket_pda};
use svm_accountant_tools_common::snapshot::{
    BalanceRow, ChainRegistrationRow, ModifyBalanceRow, TransceiverHubRow, TransceiverPeerRow,
    TransferRow,
};

use crate::batcher::TxBatch;
use crate::target::BackfillTarget;
use crate::wire::{legacy_wire_size, PACKET_DATA_SIZE};

/// Build, parse back and size-check the instruction for `batch`.
pub fn build_ix(target: &BackfillTarget, batch: &TxBatch) -> Result<Instruction> {
    let ix = match batch {
        TxBatch::BackfillNoReplay(e) => build_backfill_noreplay_ix(target, e),
        TxBatch::BackfillBalance(e) => build_backfill_balance_ix(target, e),
        TxBatch::BackfillModifyBalance(e) => build_backfill_modify_balance_ix(target, e),
        TxBatch::BackfillChainRegistration(e) => build_backfill_chain_registration_ix(target, e),
        TxBatch::BackfillTransceiverHub(e) => build_backfill_transceiver_hub_ix(target, e)?,
        TxBatch::BackfillTransceiverPeer(e) => build_backfill_transceiver_peer_ix(target, e)?,
    };
    verify_ix(target, batch, &ix)?;
    Ok(ix)
}

/// SECURITY: second decode of every instruction through the program's parser,
/// plus the account count each handler requires and the exact wire size.
fn verify_ix(target: &BackfillTarget, batch: &TxBatch, ix: &Instruction) -> Result<()> {
    let kind = batch.ix_name();
    ensure!(
        ix.program_id == target.program_id(),
        "ix program id {}",
        ix.program_id
    );
    let (&disc, body) = ix
        .data
        .split_first()
        .ok_or_else(|| anyhow!("empty ix data"))?;
    let d = target.discriminators();
    let (want_discriminator, parsed): (Option<u8>, Result<usize, GlobalAccountantError>) =
        match batch {
            TxBatch::BackfillNoReplay(_) => (
                Some(d.noreplay),
                NoReplayBatch::parse(body).map(|b| b.groups().map(|g| g.entries.len()).sum()),
            ),
            TxBatch::BackfillBalance(_) => (
                Some(d.balance),
                BalanceBatch::parse(body).map(|b| b.entries().len()),
            ),
            TxBatch::BackfillModifyBalance(_) => (
                Some(d.modify_balance),
                ModifyBalanceBatch::parse(body).map(|b| b.entries().len()),
            ),
            TxBatch::BackfillChainRegistration(_) => (
                Some(d.chain_registration),
                ChainRegistrationBatch::parse(body).map(|b| b.entries().len()),
            ),
            TxBatch::BackfillTransceiverHub(_) => (
                d.transceiver_hub,
                TransceiverHubBatch::parse(body).map(|b| b.entries().len()),
            ),
            TxBatch::BackfillTransceiverPeer(_) => (
                d.transceiver_peer,
                TransceiverPeerBatch::parse(body).map(|b| b.entries().len()),
            ),
        };
    ensure!(
        Some(disc) == want_discriminator,
        "{kind} disc {disc}, want {want_discriminator:?}"
    );
    let parsed =
        parsed.map_err(|e| anyhow!("definitions parser rejects the {kind} batch: {e:?}"))?;
    ensure!(
        parsed == batch.len(),
        "{kind} parsed {parsed} entries, batch holds {}",
        batch.len()
    );
    let want_accounts = match batch {
        TxBatch::BackfillNoReplay(e) => 4 + noreplay_buckets(e).len(),
        TxBatch::BackfillChainRegistration(e) => 2 + 2 * e.len(),
        _ => 2 + batch.len(),
    };
    ensure!(
        ix.accounts.len() == want_accounts,
        "{kind} carries {} accounts, want {want_accounts}",
        ix.accounts.len()
    );
    let size = legacy_wire_size(ix, &target.payer());
    ensure!(
        size <= PACKET_DATA_SIZE,
        "{kind} tx is {size} bytes, over {PACKET_DATA_SIZE}"
    );
    Ok(())
}

/// Distinct buckets of a sorted transfer batch, in order. One writable meta each.
fn noreplay_buckets(transfers: &[TransferRow]) -> Vec<BucketId> {
    let mut buckets: Vec<BucketId> = transfers
        .iter()
        .map(|t| bucket_id(t.emitter_key(), t.sequence))
        .collect();
    buckets.dedup();
    buckets
}

fn entry_count(len: usize) -> u8 {
    u8::try_from(len).expect("batch caps are below 256")
}

/// `count (u8) ‖ count × entry` batches. Accounts: payer, system program, then
/// the `N` writable PDAs of each entry, in entry order.
fn build_batch<T, E: Pod, const N: usize>(
    target: &BackfillTarget,
    disc: u8,
    rows: &[T],
    encode: impl Fn(&T) -> (E, [Pubkey; N]),
) -> Instruction {
    let mut data = Vec::with_capacity(2 + rows.len() * std::mem::size_of::<E>());
    data.extend([disc, entry_count(rows.len())]);
    let mut accounts = Vec::with_capacity(2 + rows.len() * N);
    accounts.push(AccountMeta::new(target.payer(), true));
    accounts.push(AccountMeta::new_readonly(target.system_program(), false));
    for row in rows {
        let (entry, pdas) = encode(row);
        data.extend_from_slice(bytemuck::bytes_of(&entry));
        accounts.extend(pdas.into_iter().map(|pda| AccountMeta::new(pda, false)));
    }
    Instruction {
        program_id: target.program_id(),
        accounts,
        data,
    }
}

/// `BackfillNoReplay`: each `(chain, emitter)` run becomes one group.
/// Accounts: payer, NoReplay program, NoReplay authority, system program, then
/// one bucket PDA per distinct bucket in order.
pub fn build_backfill_noreplay_ix(
    target: &BackfillTarget,
    transfers: &[TransferRow],
) -> Instruction {
    let groups: Vec<&[TransferRow]> = transfers
        .chunk_by(|a, b| a.emitter_key() == b.emitter_key())
        .collect();
    let mut data = vec![target.discriminators().noreplay, entry_count(groups.len())];
    for group in &groups {
        let emitter = group[0].emitter_key();
        let header = BackfillNoReplayGroupHeader::new(
            emitter.chain.0,
            emitter.emitter.0,
            entry_count(group.len()),
        );
        data.extend_from_slice(bytemuck::bytes_of(&header));
        for t in *group {
            let entry = BackfillNoReplayEntry::new(t.sequence.0, t.digest.0);
            data.extend_from_slice(bytemuck::bytes_of(&entry));
        }
    }

    let mut accounts = vec![
        AccountMeta::new(target.payer(), true),
        AccountMeta::new_readonly(target.noreplay_program(), false),
        AccountMeta::new_readonly(target.noreplay_authority(), false),
        AccountMeta::new_readonly(target.system_program(), false),
    ];
    accounts.extend(noreplay_buckets(transfers).iter().map(|bucket| {
        let pda = derive_bucket_pda(
            &target.noreplay_program(),
            &target.noreplay_authority(),
            bucket,
        );
        AccountMeta::new(pda, false)
    }));
    Instruction {
        program_id: target.program_id(),
        accounts,
        data,
    }
}

/// `BackfillBalance`: one Balance PDA per entry.
pub fn build_backfill_balance_ix(target: &BackfillTarget, rows: &[BalanceRow]) -> Instruction {
    build_batch(target, target.discriminators().balance, rows, |a| {
        let (chain, token_chain, token_address) = (a.chain.0, a.token_chain.0, a.token_address.0);
        let key = BalanceAccountKey::new(chain, token_chain, token_address);
        (
            BackfillBalanceEntry::new(chain, token_chain, token_address, a.balance),
            [derive_pda(&target.program_id(), &key)],
        )
    })
}

/// `BackfillModifyBalance`: one `ModifyBalance` record PDA per entry.
pub fn build_backfill_modify_balance_ix(
    target: &BackfillTarget,
    rows: &[ModifyBalanceRow],
) -> Instruction {
    build_batch(target, target.discriminators().modify_balance, rows, |m| {
        let entry = BackfillModifyBalanceEntry::new(
            m.modify_kind as u8,
            m.chain_id.0,
            m.token_chain.0,
            m.sequence.0,
            m.token_address.0,
            m.amount,
            m.reason,
        );
        let key = ModifyBalanceKey::new(m.sequence.0);
        (entry, [derive_pda(&target.program_id(), &key)])
    })
}

/// `BackfillChainRegistration` (WTT) / `BackfillRelayerChainRegistration` (NTT):
/// per entry, the `ChainRegistration` PDA then the `RegisterChain` record PDA.
pub fn build_backfill_chain_registration_ix(
    target: &BackfillTarget,
    rows: &[ChainRegistrationRow],
) -> Instruction {
    build_batch(
        target,
        target.discriminators().chain_registration,
        rows,
        |r| {
            (
                BackfillChainRegistrationEntry::new(
                    r.chain.0,
                    r.sequence.0,
                    r.registered_emitter.0,
                ),
                [
                    derive_pda(&target.program_id(), &ChainRegistrationKey::new(r.chain.0)),
                    derive_pda(&target.program_id(), &RegisterChainKey::new(r.sequence.0)),
                ],
            )
        },
    )
}

/// `BackfillTransceiverHub` (NTT only): one hub PDA per entry.
pub fn build_backfill_transceiver_hub_ix(
    target: &BackfillTarget,
    rows: &[TransceiverHubRow],
) -> Result<Instruction> {
    let disc = target
        .transceiver_discriminator(target.discriminators().transceiver_hub, "transceiver_hub")?;
    Ok(build_batch(target, disc, rows, |h| {
        let key = TransceiverHubKey::new(h.chain.0, h.address.0);
        (
            BackfillTransceiverHubEntry::new(
                h.chain.0,
                h.address.0,
                h.hub_chain.0,
                h.hub_address.0,
            ),
            [derive_pda(&target.program_id(), &key)],
        )
    }))
}

/// `BackfillTransceiverPeer` (NTT only): one peer PDA per entry.
pub fn build_backfill_transceiver_peer_ix(
    target: &BackfillTarget,
    rows: &[TransceiverPeerRow],
) -> Result<Instruction> {
    let disc = target
        .transceiver_discriminator(target.discriminators().transceiver_peer, "transceiver_peer")?;
    Ok(build_batch(target, disc, rows, |p| {
        let key = TransceiverPeerKey::new(p.chain.0, p.address.0, p.dest_chain.0);
        (
            BackfillTransceiverPeerEntry::new(
                p.chain.0,
                p.address.0,
                p.dest_chain.0,
                p.peer_address.0,
            ),
            [derive_pda(&target.program_id(), &key)],
        )
    }))
}

/// Build and check every batch's instruction, and fingerprint the plan:
/// SHA-256 over `(data_len_be ‖ data ‖ account keys)` per instruction, in
/// plan order. Fails on the first batch `build_ix` rejects.
pub fn plan_hash(target: &BackfillTarget, batches: &[TxBatch]) -> Result<[u8; 32]> {
    let mut hasher = Sha256::new();
    for (i, batch) in batches.iter().enumerate() {
        let ix =
            build_ix(target, batch).with_context(|| format!("batch {i} ({})", batch.ix_name()))?;
        hasher.update((ix.data.len() as u64).to_be_bytes());
        hasher.update(&ix.data);
        for meta in &ix.accounts {
            hasher.update(meta.pubkey.as_ref());
        }
    }
    Ok(hasher.finalize().into())
}
