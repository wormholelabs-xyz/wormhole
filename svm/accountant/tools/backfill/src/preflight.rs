//! Pre-submission on-chain state checks ("preflight") for the resumable
//! `run` orchestrator: re-verifies, per chunk, that state the cursor
//! claims is already confirmed is genuinely present on chain before it is
//! skipped.
//!
//! Resubmitting an already-landed chunk is not always rejected by the
//! chain: `BackfillNoReplay`'s CPI target OR-merges its bitmap
//! unconditionally, so a resubmission just succeeds again (wasting a tx
//! fee) instead of failing with a recognizable error. Dedup on resume
//! therefore has to be verified off-chain by re-reading the state each
//! chunk would write, rather than relying on the chain to reject a repeat.
//!
//! `chunk_confirmed_on_chain` only returns `Ok(true)` when every piece of
//! state a chunk would write is present and correct on chain. Any
//! ambiguity — missing account, wrong length, value mismatch — returns
//! `Ok(false)`: resubmit rather than risk silently skipping unwritten
//! state.

use anyhow::Result;
use global_accountant_definitions::{
    BalanceAccountLayout, RelayerChainRegistrationLayout, TransceiverHubLayout,
    TransceiverPeerLayout, NOREPLAY_BITMAP_BYTES, NOREPLAY_BITMAP_OFFSET, NOREPLAY_BITS_PER_BUCKET,
};
use solana_client::nonblocking::rpc_client::RpcClient;

use crate::catalogue::{
    AccountRecord, RelayerChainRegistrationRecord, TransceiverHubRecord, TransceiverPeerRecord,
    TransferRecord,
};
use crate::chunker::ChunkPlan;
use crate::tx_builder::{
    derive_balance_pda, derive_noreplay_authority_pda, derive_noreplay_bucket,
    derive_relayer_registration_pda, derive_transceiver_hub_pda, derive_transceiver_peer_pda,
    BackfillCtx,
};

/// Whether it is safe to skip resubmitting `chunk` — true only if every
/// piece of state it would write is already present and correct on chain.
/// Any doubt resolves to `Ok(false)` (resubmit), never `Ok(true)` (skip).
pub async fn chunk_confirmed_on_chain(
    rpc: &RpcClient,
    ctx: &BackfillCtx,
    chunk: &ChunkPlan,
) -> Result<bool> {
    match chunk {
        ChunkPlan::BackfillNoReplay(transfers) => noreplay_confirmed(rpc, ctx, transfers).await,
        ChunkPlan::BackfillBalance(accounts) => balance_confirmed(rpc, ctx, accounts).await,
        ChunkPlan::BackfillRelayerRegistration(entries) => {
            relayer_registration_confirmed(rpc, ctx, entries).await
        }
        ChunkPlan::BackfillTransceiverHub(entries) => {
            transceiver_hub_confirmed(rpc, ctx, entries).await
        }
        ChunkPlan::BackfillTransceiverPeer(entries) => {
            transceiver_peer_confirmed(rpc, ctx, entries).await
        }
        // Deferred chunks aren't submitted to this program at all (they
        // replay through the operational program later) — nothing to verify.
        ChunkPlan::DeferredModification(_) | ChunkPlan::DeferredRegistration(_) => Ok(true),
    }
}

async fn noreplay_confirmed(
    rpc: &RpcClient,
    ctx: &BackfillCtx,
    transfers: &[TransferRecord],
) -> Result<bool> {
    let noreplay_auth = derive_noreplay_authority_pda(&ctx.program_id);

    // Group by unique bucket so a bucket shared by several entries is
    // fetched once.
    let mut buckets: std::collections::BTreeMap<(u16, [u8; 32], u64), Vec<u64>> =
        std::collections::BTreeMap::new();
    for t in transfers {
        buckets
            .entry((t.chain, t.emitter, t.sequence / NOREPLAY_BITS_PER_BUCKET))
            .or_default()
            .push(t.sequence);
    }

    for ((chain, emitter, bucket_idx), sequences) in buckets {
        let pda = derive_noreplay_bucket(
            &noreplay_auth,
            chain,
            &emitter,
            bucket_idx * NOREPLAY_BITS_PER_BUCKET,
        );
        let Ok(account) = rpc.get_account(&pda).await else {
            return Ok(false); // bucket doesn't exist on chain yet
        };
        let bitmap_start = NOREPLAY_BITMAP_OFFSET;
        let bitmap_end = bitmap_start + NOREPLAY_BITMAP_BYTES;
        if account.data.len() < bitmap_end {
            return Ok(false); // unexpected layout — treat as not-confirmed
        }
        let bitmap = &account.data[bitmap_start..bitmap_end];
        for seq in sequences {
            let bit = (seq % NOREPLAY_BITS_PER_BUCKET) as usize;
            if bitmap[bit / 8] & (1u8 << (bit % 8)) == 0 {
                return Ok(false); // this entry's bit is not set
            }
        }
    }
    Ok(true)
}

async fn balance_confirmed(
    rpc: &RpcClient,
    ctx: &BackfillCtx,
    accounts: &[AccountRecord],
) -> Result<bool> {
    for a in accounts {
        let pda = derive_balance_pda(&ctx.program_id, a.chain, a.token_chain, &a.token_address);
        let Ok(account) = rpc.get_account(&pda).await else {
            return Ok(false);
        };
        if account.data.len() != BalanceAccountLayout::LEN {
            return Ok(false);
        }
        // Layout: tag@0, _pad0@1, chain@2, token_chain@4, token_address@6, balance@38.
        let chain = u16::from_le_bytes([account.data[2], account.data[3]]);
        let token_chain = u16::from_le_bytes([account.data[4], account.data[5]]);
        let mut token_address = [0u8; 32];
        token_address.copy_from_slice(&account.data[6..38]);
        let mut balance = [0u8; 32];
        balance.copy_from_slice(&account.data[38..70]);
        if chain != a.chain
            || token_chain != a.token_chain
            || token_address != a.token_address
            || balance != a.balance
        {
            return Ok(false);
        }
    }
    Ok(true)
}

async fn relayer_registration_confirmed(
    rpc: &RpcClient,
    ctx: &BackfillCtx,
    entries: &[RelayerChainRegistrationRecord],
) -> Result<bool> {
    for e in entries {
        let pda = derive_relayer_registration_pda(&ctx.program_id, e.chain);
        let Ok(account) = rpc.get_account(&pda).await else {
            return Ok(false);
        };
        if account.data.len() != RelayerChainRegistrationLayout::LEN {
            return Ok(false);
        }
        // Layout: tag@0, _pad0@1, chain@2, _padding@4 (28), emitter_address@32.
        let chain = u16::from_le_bytes([account.data[2], account.data[3]]);
        let mut emitter = [0u8; 32];
        emitter.copy_from_slice(&account.data[32..64]);
        if chain != e.chain || emitter != e.registered_emitter {
            return Ok(false);
        }
    }
    Ok(true)
}

async fn transceiver_hub_confirmed(
    rpc: &RpcClient,
    ctx: &BackfillCtx,
    entries: &[TransceiverHubRecord],
) -> Result<bool> {
    for e in entries {
        let pda = derive_transceiver_hub_pda(&ctx.program_id, e.chain, &e.address);
        let Ok(account) = rpc.get_account(&pda).await else {
            return Ok(false);
        };
        if account.data.len() != TransceiverHubLayout::LEN {
            return Ok(false);
        }
        // Layout: tag@0, _pad0@1, chain@2, hub_chain@4, address@6, hub_address@38.
        let chain = u16::from_le_bytes([account.data[2], account.data[3]]);
        let hub_chain = u16::from_le_bytes([account.data[4], account.data[5]]);
        let mut address = [0u8; 32];
        address.copy_from_slice(&account.data[6..38]);
        let mut hub_address = [0u8; 32];
        hub_address.copy_from_slice(&account.data[38..70]);
        if chain != e.chain
            || hub_chain != e.hub_chain
            || address != e.address
            || hub_address != e.hub_address
        {
            return Ok(false);
        }
    }
    Ok(true)
}

async fn transceiver_peer_confirmed(
    rpc: &RpcClient,
    ctx: &BackfillCtx,
    entries: &[TransceiverPeerRecord],
) -> Result<bool> {
    for e in entries {
        let pda = derive_transceiver_peer_pda(&ctx.program_id, e.chain, &e.address, e.dest_chain);
        let Ok(account) = rpc.get_account(&pda).await else {
            return Ok(false);
        };
        if account.data.len() != TransceiverPeerLayout::LEN {
            return Ok(false);
        }
        // Layout: tag@0, _pad0@1, chain@2, dest_chain@4, address@6, peer_address@38.
        let chain = u16::from_le_bytes([account.data[2], account.data[3]]);
        let dest_chain = u16::from_le_bytes([account.data[4], account.data[5]]);
        let mut address = [0u8; 32];
        address.copy_from_slice(&account.data[6..38]);
        let mut peer_address = [0u8; 32];
        peer_address.copy_from_slice(&account.data[38..70]);
        if chain != e.chain
            || dest_chain != e.dest_chain
            || address != e.address
            || peer_address != e.peer_address
        {
            return Ok(false);
        }
    }
    Ok(true)
}
