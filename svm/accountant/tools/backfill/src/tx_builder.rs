//! Build `solana_instruction::Instruction` values from chunk plans.
//!
//! Wire formats mirror the program's parsers byte-for-byte. Discriminator
//! constants are pinned to the program's `Instruction` enum via a test
//! (`tests/tx_builder.rs::discriminators_match_program_enum`) so a renumber
//! on the program side surfaces as a test failure rather than silent
//! mis-routing.

use std::str::FromStr;

use solana_instruction::{AccountMeta, Instruction};
use solana_pubkey::Pubkey;

use crate::catalogue::{
    AccountRecord, RelayerChainRegistrationRecord, TransceiverHubRecord, TransceiverPeerRecord,
    TransferRecord,
};

use global_accountant_backfill::{Instruction as ProgramIx, BACKFILL_AUTHORITY};
use global_accountant_definitions::{
    ACCOUNT_SEED_PREFIX, NOREPLAY_AUTHORITY_SEED_PREFIX, NOREPLAY_BITS_PER_BUCKET,
    NOREPLAY_PROGRAM_ID, RELAYER_CHAIN_REGISTRATION_SEED_PREFIX, TRANSCEIVER_HUB_SEED_PREFIX,
    TRANSCEIVER_PEER_SEED_PREFIX,
};
use ntt_global_accountant_backfill::Instruction as NttProgramIx;

/// Instruction discriminator for `BackfillNoReplay`. Mirrored from the
/// program's enum via const cast; the equality check lives in the test.
pub const BACKFILL_NOREPLAY_DISC: u8 = ProgramIx::BackfillNoReplay as u8;
/// Instruction discriminator for `BackfillBalance`.
pub const BACKFILL_BALANCE_DISC: u8 = ProgramIx::BackfillBalance as u8;
/// Instruction discriminator for `BackfillRelayerRegistration` (NTT program).
/// Sourced from the NTT program's `Instruction` enum, same as the WTT discs;
/// the equality check lives in `tests/tx_builder.rs`.
pub const BACKFILL_RELAYER_REGISTRATION_DISC: u8 = NttProgramIx::BackfillRelayerRegistration as u8;
/// Instruction discriminator for `BackfillTransceiverHub` (NTT program).
pub const BACKFILL_TRANSCEIVER_HUB_DISC: u8 = NttProgramIx::BackfillTransceiverHub as u8;
/// Instruction discriminator for `BackfillTransceiverPeer` (NTT program).
pub const BACKFILL_TRANSCEIVER_PEER_DISC: u8 = NttProgramIx::BackfillTransceiverPeer as u8;

/// Pubkey baked into the program's `.so` as `BACKFILL_AUTHORITY`. Re-exported
/// so the orchestrator can sanity-check the configured signer matches at
/// startup — otherwise every tx would hit `UnauthorizedCaller` on-chain.
pub fn backfill_authority_pubkey() -> Pubkey {
    Pubkey::new_from_array(BACKFILL_AUTHORITY)
}

/// Common context for every backfill ix: program id, payer, and the two
/// program-id constants the orchestrator needs to thread through.
///
/// **Invariant**: `payer` MUST equal [`backfill_authority_pubkey`] — the
/// program's `require_authority` check rejects every other signer with
/// `UnauthorizedCaller`. Callers construct `BackfillCtx` via [`BackfillCtx::new`]
/// which enforces this at construction time.
#[derive(Debug, Clone)]
pub struct BackfillCtx {
    pub program_id: Pubkey,
    pub payer: Pubkey,
    pub system_program: Pubkey,
    pub noreplay_program: Pubkey,
}

impl BackfillCtx {
    /// Construct a context. Panics if `payer` does not match the program's
    /// compile-time `BACKFILL_AUTHORITY` const — sending txs with any other
    /// signer is guaranteed to fail on-chain, so we surface the misconfig as
    /// soon as it can be detected.
    pub fn new(program_id: Pubkey, payer: Pubkey) -> Self {
        assert_eq!(
            payer,
            backfill_authority_pubkey(),
            "payer {payer} does not match BACKFILL_AUTHORITY {}; the on-chain program will reject every tx",
            backfill_authority_pubkey()
        );
        Self {
            program_id,
            payer,
            system_program: Pubkey::from_str("11111111111111111111111111111111")
                .expect("hardcoded system program id parses"),
            noreplay_program: Pubkey::new_from_array(NOREPLAY_PROGRAM_ID),
        }
    }
}

// ============================================================================
// PDA derivations
// ============================================================================

pub fn derive_noreplay_authority_pda(program_id: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(&[NOREPLAY_AUTHORITY_SEED_PREFIX], program_id).0
}

/// Derive the canonical noreplay bitmap PDA for `(authority, chain, emitter,
/// sequence)`. Mirrors `solana_noreplay::pda::BitmapPdaSeeds`:
/// namespace = `chain_be ‖ emitter` (34 bytes), split at 32 for the cw-style
/// two-chunk seed structure; the fourth seed is `(sequence / 1024).to_le_bytes()`.
pub fn derive_noreplay_bucket(
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

pub fn derive_balance_pda(
    program_id: &Pubkey,
    chain: u16,
    token_chain: u16,
    token_address: &[u8; 32],
) -> Pubkey {
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

/// Derive the canonical relayer-chain-registration PDA. Seeds:
/// `(RELAYER_CHAIN_REGISTRATION_SEED_PREFIX, chain_be)`.
pub fn derive_relayer_registration_pda(program_id: &Pubkey, chain: u16) -> Pubkey {
    Pubkey::find_program_address(
        &[RELAYER_CHAIN_REGISTRATION_SEED_PREFIX, &chain.to_be_bytes()],
        program_id,
    )
    .0
}

/// Derive the canonical transceiver-hub PDA. Seeds:
/// `(TRANSCEIVER_HUB_SEED_PREFIX, chain_be, address)`.
pub fn derive_transceiver_hub_pda(program_id: &Pubkey, chain: u16, address: &[u8; 32]) -> Pubkey {
    Pubkey::find_program_address(
        &[TRANSCEIVER_HUB_SEED_PREFIX, &chain.to_be_bytes(), address],
        program_id,
    )
    .0
}

/// Derive the canonical transceiver-peer PDA. Seeds:
/// `(TRANSCEIVER_PEER_SEED_PREFIX, chain_be, address, dest_chain_be)`.
pub fn derive_transceiver_peer_pda(
    program_id: &Pubkey,
    chain: u16,
    address: &[u8; 32],
    dest_chain: u16,
) -> Pubkey {
    Pubkey::find_program_address(
        &[
            TRANSCEIVER_PEER_SEED_PREFIX,
            &chain.to_be_bytes(),
            address,
            &dest_chain.to_be_bytes(),
        ],
        program_id,
    )
    .0
}

// ============================================================================
// Instruction builders
// ============================================================================

/// Build a `BackfillNoReplay` ix from a slice of transfers. Groups
/// consecutively-equal `(chain, emitter)` runs into the program's compact
/// emitter-grouped wire format. Account list includes one bucket PDA per
/// unique `(chain, emitter, sequence/1024)` encountered, in walk order.
///
/// Caller responsibility (NOT enforced here): the chunker must already have
/// constrained the input to be safe under the wire-size budget — see the
/// `MAX_NOREPLAY_ENTRIES_PER_CHUNK` empirical limit in `chunker.rs`.
pub fn build_backfill_noreplay_ix(ctx: &BackfillCtx, transfers: &[TransferRecord]) -> Instruction {
    // ----- Group by (chain, emitter) preserving order -----
    let mut groups: Vec<Vec<&TransferRecord>> = Vec::new();
    let mut current: Vec<&TransferRecord> = Vec::new();
    let mut current_key: Option<(u16, [u8; 32])> = None;
    for t in transfers {
        let key = (t.chain, t.emitter);
        if current_key != Some(key) {
            if !current.is_empty() {
                groups.push(std::mem::take(&mut current));
            }
            current_key = Some(key);
        }
        current.push(t);
    }
    if !current.is_empty() {
        groups.push(current);
    }

    // ----- Wire data: [disc][group_count] [chain emitter ec [seq digest]...]... -----
    let mut data = Vec::with_capacity(2 + groups.iter().map(|g| 35 + g.len() * 40).sum::<usize>());
    data.push(BACKFILL_NOREPLAY_DISC);
    data.push(groups.len() as u8);
    for g in &groups {
        let first = g[0];
        data.extend_from_slice(&first.chain.to_be_bytes());
        data.extend_from_slice(&first.emitter);
        data.push(g.len() as u8);
        for t in g {
            data.extend_from_slice(&t.sequence.to_be_bytes());
            data.extend_from_slice(&t.digest);
        }
    }

    // ----- Accounts: 4 fixed + N unique bucket PDAs -----
    let noreplay_auth = derive_noreplay_authority_pda(&ctx.program_id);
    let mut accounts = vec![
        AccountMeta::new(ctx.payer, true),
        AccountMeta::new_readonly(ctx.noreplay_program, false),
        AccountMeta::new_readonly(noreplay_auth, false),
        AccountMeta::new_readonly(ctx.system_program, false),
    ];
    let mut prev_bucket: Option<(u16, [u8; 32], u64)> = None;
    for t in transfers {
        let bucket_idx = t.sequence / NOREPLAY_BITS_PER_BUCKET;
        let cur = (t.chain, t.emitter, bucket_idx);
        if Some(cur) != prev_bucket {
            let pda = derive_noreplay_bucket(&noreplay_auth, t.chain, &t.emitter, t.sequence);
            accounts.push(AccountMeta::new(pda, false));
            prev_bucket = Some(cur);
        }
    }

    Instruction {
        program_id: ctx.program_id,
        accounts,
        data,
    }
}

/// Build a `BackfillBalance` ix from a slice of account records. Wire is
/// flat: each entry repeats `(chain, token_chain, token_address, balance)`
/// since balance PDAs are inherently per-entry unique (no dedup possible).
pub fn build_backfill_balance_ix(ctx: &BackfillCtx, accounts: &[AccountRecord]) -> Instruction {
    let mut data = Vec::with_capacity(2 + accounts.len() * 68);
    data.push(BACKFILL_BALANCE_DISC);
    data.push(accounts.len() as u8);
    for a in accounts {
        data.extend_from_slice(&a.chain.to_be_bytes());
        data.extend_from_slice(&a.token_chain.to_be_bytes());
        data.extend_from_slice(&a.token_address);
        data.extend_from_slice(&a.balance);
    }

    let mut metas = vec![
        AccountMeta::new(ctx.payer, true),
        AccountMeta::new_readonly(ctx.system_program, false),
    ];
    for a in accounts {
        let pda = derive_balance_pda(&ctx.program_id, a.chain, a.token_chain, &a.token_address);
        metas.push(AccountMeta::new(pda, false));
    }

    Instruction {
        program_id: ctx.program_id,
        accounts: metas,
        data,
    }
}

/// Build a `BackfillRelayerRegistration` ix from a slice of relayer
/// registrations. Wire is flat: `[disc][count]` then per entry `chain(2 BE) ‖
/// emitter_address(32)` (34 B/entry). One writable registration PDA meta per
/// entry, in entry order.
///
/// Caller responsibility (NOT enforced here): the chunker must supply a batch
/// within `MAX_RELAYER_REGISTRATION_ENTRIES_PER_CHUNK` that is strictly
/// ascending by `chain` — the on-chain handler rejects non-increasing keys.
pub fn build_backfill_relayer_registration_ix(
    ctx: &BackfillCtx,
    entries: &[RelayerChainRegistrationRecord],
) -> Instruction {
    let mut data = Vec::with_capacity(2 + entries.len() * 34);
    data.push(BACKFILL_RELAYER_REGISTRATION_DISC);
    data.push(entries.len() as u8);
    for e in entries {
        data.extend_from_slice(&e.chain.to_be_bytes());
        data.extend_from_slice(&e.registered_emitter);
    }

    let mut metas = vec![
        AccountMeta::new(ctx.payer, true),
        AccountMeta::new_readonly(ctx.system_program, false),
    ];
    for e in entries {
        let pda = derive_relayer_registration_pda(&ctx.program_id, e.chain);
        metas.push(AccountMeta::new(pda, false));
    }

    Instruction {
        program_id: ctx.program_id,
        accounts: metas,
        data,
    }
}

/// Build a `BackfillTransceiverHub` ix. Wire: `[disc][count]` then per entry
/// `chain(2 BE) ‖ address(32) ‖ hub_chain(2 BE) ‖ hub_address(32)` (68
/// B/entry). One writable hub PDA meta per entry, in entry order.
///
/// Caller responsibility: batch within `MAX_TRANSCEIVER_HUB_ENTRIES_PER_CHUNK`,
/// strictly ascending by `(chain, address)`.
pub fn build_backfill_transceiver_hub_ix(
    ctx: &BackfillCtx,
    entries: &[TransceiverHubRecord],
) -> Instruction {
    let mut data = Vec::with_capacity(2 + entries.len() * 68);
    data.push(BACKFILL_TRANSCEIVER_HUB_DISC);
    data.push(entries.len() as u8);
    for e in entries {
        data.extend_from_slice(&e.chain.to_be_bytes());
        data.extend_from_slice(&e.address);
        data.extend_from_slice(&e.hub_chain.to_be_bytes());
        data.extend_from_slice(&e.hub_address);
    }

    let mut metas = vec![
        AccountMeta::new(ctx.payer, true),
        AccountMeta::new_readonly(ctx.system_program, false),
    ];
    for e in entries {
        let pda = derive_transceiver_hub_pda(&ctx.program_id, e.chain, &e.address);
        metas.push(AccountMeta::new(pda, false));
    }

    Instruction {
        program_id: ctx.program_id,
        accounts: metas,
        data,
    }
}

/// Build a `BackfillTransceiverPeer` ix. Wire: `[disc][count]` then per entry
/// `chain(2 BE) ‖ address(32) ‖ dest_chain(2 BE) ‖ peer_address(32)` (68
/// B/entry). One writable peer PDA meta per entry, in entry order.
///
/// Caller responsibility: batch within `MAX_TRANSCEIVER_PEER_ENTRIES_PER_CHUNK`,
/// strictly ascending by `(chain, address, dest_chain)`.
pub fn build_backfill_transceiver_peer_ix(
    ctx: &BackfillCtx,
    entries: &[TransceiverPeerRecord],
) -> Instruction {
    let mut data = Vec::with_capacity(2 + entries.len() * 68);
    data.push(BACKFILL_TRANSCEIVER_PEER_DISC);
    data.push(entries.len() as u8);
    for e in entries {
        data.extend_from_slice(&e.chain.to_be_bytes());
        data.extend_from_slice(&e.address);
        data.extend_from_slice(&e.dest_chain.to_be_bytes());
        data.extend_from_slice(&e.peer_address);
    }

    let mut metas = vec![
        AccountMeta::new(ctx.payer, true),
        AccountMeta::new_readonly(ctx.system_program, false),
    ];
    for e in entries {
        let pda = derive_transceiver_peer_pda(&ctx.program_id, e.chain, &e.address, e.dest_chain);
        metas.push(AccountMeta::new(pda, false));
    }

    Instruction {
        program_id: ctx.program_id,
        accounts: metas,
        data,
    }
}
