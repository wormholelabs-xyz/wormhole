//! Build `solana_instruction::Instruction` values from batch plans.
//!
//! Entries are encoded through the definitions entry types
//! (`global_accountant_definitions::backfill_ix_data`), the types the program
//! parses with. [`build_ix`] then parses the data back with the program's own
//! batch parser and measures the exact legacy wire size, so a malformed batch
//! fails here, before any transaction is sent.

use anyhow::{anyhow, ensure, Result};
use bytemuck::Pod;
use solana_hash::Hash;
use solana_instruction::{AccountMeta, Instruction};
use solana_message::Message;
use solana_pubkey::Pubkey;
use solana_transaction::Transaction;

use global_accountant_definitions::{
    BackfillBalanceEntry, BackfillChainRegistrationEntry, BackfillModifyBalanceEntry,
    BackfillNoReplayEntry, BackfillNoReplayGroupHeader, BackfillTransceiverHubEntry,
    BackfillTransceiverPeerEntry, BalanceAccountKey, BalanceBatch, ChainRegistrationBatch,
    ChainRegistrationKey, GlobalAccountantError, ModifyBalanceBatch, ModifyBalanceKey,
    NoReplayBatch, RegisterChainKey, TransceiverHubBatch, TransceiverHubKey, TransceiverPeerBatch,
    TransceiverPeerKey, Uint256, NOREPLAY_PROGRAM_ID,
};

use crate::accountant::{Accountant, Discriminators};
use crate::batcher::{TxBatch, PACKET_DATA_SIZE};
use crate::expected_accounts::derive_pda;
use crate::noreplay::{bucket_of, derive_bucket_pda, derive_noreplay_authority_pda, BucketId};
use crate::snapshot::{
    BalanceRow, ChainRegistrationRow, ModifyBalanceRow, TransceiverHubRow, TransceiverPeerRow,
    TransferRow,
};

/// Common context for every backfill ix.
///
/// **Invariant**: `program_id` equals the accountant's `declare_id!` address and
/// `payer` equals the accountant's backfill authority. [`BackfillTarget::try_new`]
/// checks both; the program rejects any other value.
#[derive(Debug, Clone)]
pub struct BackfillTarget {
    pub accountant: Accountant,
    pub program_id: Pubkey,
    pub payer: Pubkey,
    pub system_program: Pubkey,
    pub noreplay_program: Pubkey,
    /// `noreplay_authority` PDA of `program_id`.
    pub noreplay_authority: Pubkey,
    discriminators: Discriminators,
}

impl BackfillTarget {
    /// Fails on a program id or payer the `.so` would reject: every transaction
    /// would fail on-chain, so the misconfiguration states before any fee is paid.
    pub fn try_new(accountant: Accountant, program_id: Pubkey, payer: Pubkey) -> Result<Self> {
        ensure!(
            program_id == accountant.program_id(),
            "--program-id {program_id} is not the {accountant} backfill `declare_id!` {}",
            accountant.program_id()
        );
        ensure!(
            payer == accountant.authority(),
            "payer {payer} is not the {accountant} backfill authority {}; the program rejects every tx",
            accountant.authority()
        );
        Ok(Self {
            accountant,
            program_id,
            payer,
            system_program: Pubkey::new_from_array([0u8; 32]),
            noreplay_program: Pubkey::new_from_array(NOREPLAY_PROGRAM_ID),
            noreplay_authority: derive_noreplay_authority_pda(&program_id),
            discriminators: accountant.discriminators(),
        })
    }

    /// [`Self::try_new`] for fixed test values. Panics on a mismatch.
    pub fn new(accountant: Accountant, program_id: Pubkey, payer: Pubkey) -> Self {
        Self::try_new(accountant, program_id, payer).unwrap_or_else(|e| panic!("{e:#}"))
    }

    /// `Some` on NTT only: the transceiver maps are NTT state.
    fn transceiver_disc(&self, disc: Option<u8>, kind: &str) -> Result<u8> {
        disc.ok_or_else(|| anyhow!("{kind} rows on a {} accountant", self.accountant))
    }
}

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
    check_ix(target, batch, &ix)?;
    Ok(ix)
}

/// SECURITY: second decode of every instruction through the program's parser,
/// plus the account count each handler requires and the exact wire size.
fn check_ix(target: &BackfillTarget, batch: &TxBatch, ix: &Instruction) -> Result<()> {
    let kind = batch.kind();
    ensure!(
        ix.program_id == target.program_id,
        "ix program id {}",
        ix.program_id
    );
    let (&disc, body) = ix
        .data
        .split_first()
        .ok_or_else(|| anyhow!("empty ix data"))?;
    let d = &target.discriminators;
    let (want_disc, parsed): (Option<u8>, Result<usize, GlobalAccountantError>) = match batch {
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
        Some(disc) == want_disc,
        "{kind} disc {disc}, want {want_disc:?}"
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
    let size = legacy_wire_size(ix, &target.payer);
    ensure!(
        size <= PACKET_DATA_SIZE,
        "{kind} tx is {size} bytes, over {PACKET_DATA_SIZE}"
    );
    Ok(())
}

/// Serialized size of a one-signature legacy transaction carrying `ix`.
pub fn legacy_wire_size(ix: &Instruction, payer: &Pubkey) -> usize {
    let message =
        Message::new_with_blockhash(std::slice::from_ref(ix), Some(payer), &Hash::default());
    bincode::serialize(&Transaction::new_unsigned(message))
        .expect("serializing a well-formed Transaction cannot fail")
        .len()
}

/// Distinct buckets of a sorted transfer batch, in order. One writable meta each.
fn noreplay_buckets(transfers: &[TransferRow]) -> Vec<BucketId> {
    let mut buckets: Vec<BucketId> = transfers.iter().map(bucket_of).collect();
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
    accounts.push(AccountMeta::new(target.payer, true));
    accounts.push(AccountMeta::new_readonly(target.system_program, false));
    for row in rows {
        let (entry, pdas) = encode(row);
        data.extend_from_slice(bytemuck::bytes_of(&entry));
        accounts.extend(pdas.into_iter().map(|pda| AccountMeta::new(pda, false)));
    }
    Instruction {
        program_id: target.program_id,
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
        .chunk_by(|a, b| (a.chain, a.emitter) == (b.chain, b.emitter))
        .collect();
    let mut data = vec![target.discriminators.noreplay, entry_count(groups.len())];
    for group in &groups {
        let (chain, emitter) = (group[0].chain, group[0].emitter);
        let header = BackfillNoReplayGroupHeader::new(chain, emitter, entry_count(group.len()));
        data.extend_from_slice(bytemuck::bytes_of(&header));
        for t in *group {
            let entry = BackfillNoReplayEntry::new(t.sequence, t.digest);
            data.extend_from_slice(bytemuck::bytes_of(&entry));
        }
    }

    let mut accounts = vec![
        AccountMeta::new(target.payer, true),
        AccountMeta::new_readonly(target.noreplay_program, false),
        AccountMeta::new_readonly(target.noreplay_authority, false),
        AccountMeta::new_readonly(target.system_program, false),
    ];
    accounts.extend(noreplay_buckets(transfers).iter().map(|bucket| {
        let pda = derive_bucket_pda(&target.noreplay_program, &target.noreplay_authority, bucket);
        AccountMeta::new(pda, false)
    }));
    Instruction {
        program_id: target.program_id,
        accounts,
        data,
    }
}

/// `BackfillBalance`: one Balance PDA per entry.
pub fn build_backfill_balance_ix(target: &BackfillTarget, rows: &[BalanceRow]) -> Instruction {
    build_batch(target, target.discriminators.balance, rows, |a| {
        let balance = Uint256::from_be_bytes(a.balance);
        let key = BalanceAccountKey::new(a.chain, a.token_chain, a.token_address);
        (
            BackfillBalanceEntry::new(a.chain, a.token_chain, a.token_address, balance),
            [derive_pda(&target.program_id, &key)],
        )
    })
}

/// `BackfillModifyBalance`: one `ModifyBalance` record PDA per entry.
pub fn build_backfill_modify_balance_ix(
    target: &BackfillTarget,
    rows: &[ModifyBalanceRow],
) -> Instruction {
    build_batch(target, target.discriminators.modify_balance, rows, |m| {
        let entry = BackfillModifyBalanceEntry::new(
            m.modify_kind as u8,
            m.chain_id,
            m.token_chain,
            m.sequence,
            m.token_address,
            Uint256::from_be_bytes(m.amount),
            m.reason,
        );
        let key = ModifyBalanceKey::new(m.sequence);
        (entry, [derive_pda(&target.program_id, &key)])
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
        target.discriminators.chain_registration,
        rows,
        |r| {
            (
                BackfillChainRegistrationEntry::new(r.chain, r.sequence, r.registered_emitter),
                [
                    derive_pda(&target.program_id, &ChainRegistrationKey::new(r.chain)),
                    derive_pda(&target.program_id, &RegisterChainKey::new(r.sequence)),
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
    let disc = target.transceiver_disc(target.discriminators.transceiver_hub, "transceiver_hub")?;
    Ok(build_batch(target, disc, rows, |h| {
        let key = TransceiverHubKey::new(h.chain, h.address);
        (
            BackfillTransceiverHubEntry::new(h.chain, h.address, h.hub_chain, h.hub_address),
            [derive_pda(&target.program_id, &key)],
        )
    }))
}

/// `BackfillTransceiverPeer` (NTT only): one peer PDA per entry.
pub fn build_backfill_transceiver_peer_ix(
    target: &BackfillTarget,
    rows: &[TransceiverPeerRow],
) -> Result<Instruction> {
    let disc =
        target.transceiver_disc(target.discriminators.transceiver_peer, "transceiver_peer")?;
    Ok(build_batch(target, disc, rows, |p| {
        let key = TransceiverPeerKey::new(p.chain, p.address, p.dest_chain);
        (
            BackfillTransceiverPeerEntry::new(p.chain, p.address, p.dest_chain, p.peer_address),
            [derive_pda(&target.program_id, &key)],
        )
    }))
}

/// Build and check every batch's instruction, and fingerprint the plan:
/// SHA-256 over `(data_len_be ‖ data ‖ account keys)` per instruction, in
/// plan order. Fails on the first batch `build_ix` rejects.
pub fn batches_hash(target: &BackfillTarget, batches: &[TxBatch]) -> Result<[u8; 32]> {
    use anyhow::Context;
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    for (i, batch) in batches.iter().enumerate() {
        let ix =
            build_ix(target, batch).with_context(|| format!("batch {i} ({})", batch.kind()))?;
        hasher.update((ix.data.len() as u64).to_be_bytes());
        hasher.update(&ix.data);
        for meta in &ix.accounts {
            hasher.update(meta.pubkey.as_ref());
        }
    }
    Ok(hasher.finalize().into())
}
