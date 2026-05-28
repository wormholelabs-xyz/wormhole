//! `submit_vaas` — permissionless signed-VAA backfill.
//!
//! Port of the CosmWasm `handle_tokenbridge_vaa` path
//! (`cosmwasm/contracts/global-accountant/src/contract.rs:442-495`). Consumes
//! a fully-signed VAA via the Verify VAA Shim CPI and applies its balance
//! effects directly, **bypassing the per-`(chain, emitter, sequence, digest)`
//! quorum tracker entirely**. The instruction is the operational escape
//! hatch for stuck pending buckets and the unblock path for migration
//! backfill.
//!
//! ## Why this exists alongside `submit_observations`
//!
//! `submit_observations` accumulates per-guardian observations into a pending
//! PDA until 13/19 quorum, at which point it CPIs `MarkUsed` + opens the
//! DigestAccount + applies balance work. That path is the primary one in
//! steady state.
//!
//! `submit_vaas` is the secondary path: the caller already holds a real
//! 13-signature VAA (because it was committed previously on Wormchain, came
//! out of the migration snapshot, or was reconstructed off-chain) and wants
//! to apply its effects without re-running the per-observation accumulator.
//! The Shim verifies the quorum cryptographically against caller-posted
//! signatures, so the only on-chain state we touch is NoReplay + the
//! DigestAccount + the source / destination Account PDAs.
//!
//! Orthogonality with the quorum tracker: NoReplay is the only shared state,
//! and it is set the same way by both paths.
//!
//! ## Flow
//!
//! 1. Parse wire data: `guardian_set_bump` + `body_len` + body bytes.
//! 2. Compute `digest = keccak256(keccak256(body))`.
//! 3. CPI to Verify VAA Shim's `VerifyHash` — proves a 13-guardian quorum
//!    signed `digest`. Reuses the same helper shape as `close_digest`.
//! 4. Parse VAA body header → `(emitter_chain, emitter_address, sequence)`.
//! 5. NoReplay pre-check via `noreplay::is_marked`. Already-marked ⇒
//!    `AlreadyAccounted` (CosmWasm's `DIGESTS DuplicateMessage` short-circuit).
//! 6. Decode payload via `parse_token_bridge_payload`. Transfer ⇒
//!    `apply_transfer`; Attest / Other ⇒ no balance work.
//! 7. Flip NoReplay via `noreplay::mark_used` (real CPI in prod, sentinel
//!    write under `mock-noreplay`).
//! 8. Open the DigestAccount PDA via `open_digest_inner` so subsequent
//!    observations for the same `(chain, emitter, sequence)` see "already
//!    committed" via the same on-chain breadcrumb as the quorum path.
//!
//! All eight steps execute atomically — Solana txs are all-or-nothing — so a
//! balance overflow / underflow / Shim rejection unwinds every mutation,
//! including any lazy-init of the destination Account PDA.
//!
//! ## Deliberate divergence from CosmWasm
//!
//! - CosmWasm checks `DIGESTS[(chain, emitter, seq)]` for a pre-existing
//!   entry that *matches* (idempotent) or *differs* (`DigestMismatch`). The
//!   Solana port uses NoReplay for the same purpose: once NoReplay marks
//!   `(chain, emitter, seq)`, any caller — observations or VAA — is rejected
//!   as `AlreadyAccounted`. NoReplay is keyed on `(chain, emitter, seq)`
//!   alone (no digest), so a different-digest replay reaches the same bit
//!   and is rejected without needing the digest comparison.
//! - CosmWasm dispatches Token Bridge governance VAAs through this same
//!   `submit_vaas` entrypoint. The Solana port leaves those to a follow-on
//!   slice (`handle_tokenbridge_governance` + `modify_balance`); the payload
//!   parser already treats action 0x02 (Attest) and unknown actions as
//!   no-op-balance-work, so a governance VAA submitted today will commit the
//!   DigestAccount + flip NoReplay without applying any state change. That's
//!   safe — governance VAAs are idempotent by design and the follow-on slice
//!   will replay them once it lands.

use pinocchio::{error::ProgramError, AccountView, Address, ProgramResult};

#[cfg(not(feature = "mock-vaa"))]
use pinocchio::instruction::{InstructionAccount, InstructionView};

use crate::definitions::{
    parse_token_bridge_payload, GlobalAccountantError, TokenBridgeAction, DIGEST_SEED_PREFIX,
};
#[cfg(not(feature = "mock-vaa"))]
use crate::definitions::{VERIFY_HASH_DATA_LEN, VERIFY_HASH_SELECTOR};
use crate::err;
use crate::instructions::{noreplay, open_digest_inner, transfer::apply_transfer};

// ============================================================================
// Wire format
// ============================================================================

/// Wire format for the `submit_vaas` instruction data (after the 1-byte
/// dispatch discriminator):
///
/// | offset | size     | field             |
/// |--------|----------|-------------------|
/// | 0      | 1        | guardian_set_bump |
/// | 1      | 2        | body_len (LE)     |
/// | 3      | body_len | body              |
///
/// `guardian_set_bump` is passed straight to the Shim's `VerifyHash` so the
/// Shim can re-derive the Core Bridge's `GuardianSet` PDA on-chain without
/// paying the `find_program_address` cost. The Shim doc-string treats it as
/// trusted client input — the runtime verifies the supplied account address
/// matches the bump-derived expectation.
const SUBMIT_VAAS_FIXED_LEN: usize = 1 + 2;

/// Maximum VAA body size accepted on the wire. Same 4 KiB ceiling as
/// `submit_observations::SUBMIT_BODY_MAX` — far above the observed mainnet
/// payload size (~1 KiB) and keeps the instruction data inside the Solana
/// 1232-byte tx envelope when combined with the account list.
pub const SUBMIT_VAAS_BODY_MAX: usize = 4096;

/// Byte offsets within a VAA body (51-byte header per
/// `whitepapers/0001_generic_message_passing.md`):
///
/// | offset | size | field             |
/// |--------|------|-------------------|
/// | 0      | 4    | timestamp (u32 BE)|
/// | 4      | 4    | nonce (u32 BE)    |
/// | 8      | 2    | emitter_chain     |
/// | 10     | 32   | emitter_address   |
/// | 42     | 8    | sequence (u64 BE) |
/// | 50     | 1    | consistency_level |
const BODY_EMITTER_CHAIN_OFFSET: usize = 8;
const BODY_EMITTER_ADDRESS_OFFSET: usize = 10;
const BODY_SEQUENCE_OFFSET: usize = 42;
const BODY_HEADER_LEN: usize = 51;

pub fn process(
    program_id: &Address,
    accounts: &mut [AccountView],
    data: &[u8],
) -> ProgramResult {
    // ----- (1) Parse wire data -----
    if data.len() < SUBMIT_VAAS_FIXED_LEN {
        return Err(err(GlobalAccountantError::InvalidInstructionData));
    }
    let guardian_set_bump = data[0];
    let body_len = u16::from_le_bytes([data[1], data[2]]) as usize;
    if !(BODY_HEADER_LEN + 1..=SUBMIT_VAAS_BODY_MAX).contains(&body_len)
        || data.len() != SUBMIT_VAAS_FIXED_LEN + body_len
    {
        return Err(err(GlobalAccountantError::InvalidInstructionData));
    }
    let body_bytes = &data[SUBMIT_VAAS_FIXED_LEN..SUBMIT_VAAS_FIXED_LEN + body_len];

    // ----- (2) Compute digest -----
    let digest = double_keccak256(body_bytes);

    // Accounts:
    //   0. `[WRITE, SIGNER]` submitter — fee / rent payer for any lazy PDAs
    //                       (Account PDAs on the Transfer branch, the
    //                       DigestAccount, and the noreplay bitmap PDA on
    //                       first touch).
    //   1. `[]`              Verify VAA Shim program (CPI target).
    //   2. `[]`              Core Bridge `GuardianSet` PDA (read by Shim).
    //   3. `[]`              `GuardianSignatures` PDA (caller posted via the
    //                       Shim's `PostSignatures` before calling this ix).
    //   4. `[WRITE]`         DigestAccount PDA — opens lazily on commit.
    //   5. `[WRITE]`         NoReplay bitmap PDA — pre-check (direct read)
    //                       then write via CPI on commit.
    //   6. `[]`              NoReplay program (CPI target).
    //   7. `[]`              NoReplay authority PDA owned by this program.
    //   8. `[WRITE]`         source-chain Account PDA at
    //                       `(b"account", source_chain, token_chain, token_address)`.
    //                       Lazy-init OK. Untouched for Attest / Other payloads;
    //                       sentinel (e.g. noreplay-authority) is acceptable.
    //   9. `[WRITE]`         dest-chain Account PDA at
    //                       `(b"account", recipient_chain, token_chain, token_address)`.
    //                       Same semantics as slot 8.
    //  10. `[]`              system program — `CreateAccount` / `Allocate` /
    //                       `Assign` for lazy-inits.
    let [
        submitter,
        verify_vaa_shim_program,
        guardian_set,
        guardian_signatures,
        digest_pda,
        noreplay_bucket,
        noreplay_program,
        noreplay_authority,
        source_account_pda,
        dest_account_pda,
        _system_program,
    ] = accounts
    else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    if !submitter.is_signer() {
        return Err(ProgramError::MissingRequiredSignature);
    }

    // ----- (3) Shim CPI to verify the digest against the posted sigs -----
    //
    // The mock branch under `mock-vaa` accepts the digest verbatim — same
    // contract as `close_digest`. Production builds CPI into the Shim
    // (`EFaNWErqAtVWufdNb7yofSHHfWFos843DFpu4JBw24at`) which recovers the
    // guardian pubkeys from the supplied `GuardianSignatures` PDA and asserts
    // they reach quorum against the supplied `GuardianSet`.
    verify_vaa(
        verify_vaa_shim_program,
        guardian_set,
        guardian_signatures,
        &digest,
        guardian_set_bump,
    )?;

    // ----- (4) Parse the body header -----
    //
    // body_len ≥ 52 was already enforced above, so all three slices are safe.
    let chain = u16::from_be_bytes([
        body_bytes[BODY_EMITTER_CHAIN_OFFSET],
        body_bytes[BODY_EMITTER_CHAIN_OFFSET + 1],
    ]);
    let mut emitter = [0u8; 32];
    emitter.copy_from_slice(
        &body_bytes[BODY_EMITTER_ADDRESS_OFFSET..BODY_EMITTER_ADDRESS_OFFSET + 32],
    );
    let mut sequence_bytes = [0u8; 8];
    sequence_bytes
        .copy_from_slice(&body_bytes[BODY_SEQUENCE_OFFSET..BODY_SEQUENCE_OFFSET + 8]);
    let sequence = u64::from_be_bytes(sequence_bytes);

    // ----- (5) NoReplay pre-check -----
    //
    // CosmWasm `handle_vaa` cross-checks `DIGESTS[(chain, emitter, seq)]` and
    // short-circuits on a match (`DuplicateMessage` — see
    // `contract.rs:324-333`). The Solana port collapses the digest-table
    // check into NoReplay: any `(chain, emitter, seq)` that already passed
    // through either path (observations-quorum OR a prior `submit_vaas`) is
    // marked, so a replay reaches this branch.
    if noreplay::is_marked(
        noreplay_bucket,
        noreplay_authority.address(),
        chain,
        &emitter,
        sequence,
    )? {
        return Err(err(GlobalAccountantError::AlreadyAccounted));
    }

    // ----- (6) Parse Token Bridge payload + apply balance work -----
    //
    // CosmWasm `handle_tokenbridge_vaa` parses the payload as
    // `wormhole_sdk::token::Message::{Transfer, TransferWithPayload}` and
    // calls `accountant::commit_transfer`. The Solana port routes through
    // the same helper as `submit_observations`' quorum-completing branch —
    // shared via `instructions::transfer::apply_transfer`.
    //
    // Attest / Other payloads: no balance work, but the rest of the commit
    // (NoReplay flip + DigestAccount open) still runs. This matches
    // CosmWasm's behaviour for any payload other than 0x01 / 0x03 — those
    // come through this function but `handle_tokenbridge_vaa` short-circuits
    // before the `commit_transfer` call.
    match parse_token_bridge_payload(body_bytes).map_err(err)? {
        TokenBridgeAction::Transfer {
            amount,
            token_chain,
            token_address,
            recipient_chain,
        } => {
            apply_transfer(
                program_id,
                submitter,
                source_account_pda,
                dest_account_pda,
                chain,
                recipient_chain,
                token_chain,
                &token_address,
                amount,
            )?;
        }
        TokenBridgeAction::Attest | TokenBridgeAction::Other => {
            // Same sentinel-slot convention as `submit_observations` —
            // callers may pass the noreplay-authority PDA in slots 8 and 9.
        }
    }

    // ----- (7) NoReplay mark-used CPI -----
    //
    // After the (potentially failing) balance work succeeds, claim the
    // `(chain, emitter, seq)` slot in NoReplay. A racing tx that flipped the
    // same bit between our pre-check and this CPI surfaces as
    // `NoReplayCpiFailed`, which is the runtime's escape valve.
    noreplay::mark_used(
        submitter,
        noreplay_bucket,
        noreplay_program,
        noreplay_authority,
        _system_program,
        program_id,
        chain,
        &emitter,
        sequence,
    )?;

    // ----- (8) Open the DigestAccount PDA -----
    //
    // Same on-chain breadcrumb the quorum path leaves — keeps the
    // `close_digest` / `close_pending` ecosystem consistent across both
    // commit paths. The DigestAccount records `guardian_set_index = 0` as a
    // sentinel since `submit_vaas` does not pin a single set (the Shim
    // accepts any currently-active one); the `quorum_at_slot` is the current
    // slot, same as the observations path.
    //
    // Bump is recomputed inline rather than threaded through the wire
    // format. `submit_vaas` is the cold path (one tx per backfill, not
    // per-observation), so the ~1.5K CU cost of the `find_program_address`
    // call is irrelevant.
    let chain_be = chain.to_be_bytes();
    let sequence_be = sequence.to_be_bytes();
    let (_expected_digest_pda, digest_bump) = Address::find_program_address(
        &[DIGEST_SEED_PREFIX, &chain_be, &emitter, &sequence_be],
        program_id,
    );
    open_digest_inner(
        program_id,
        submitter,
        digest_pda,
        chain_be,
        emitter,
        sequence_be,
        digest,
        // `submit_vaas` does not track a guardian-set index. The Shim
        // accepts any currently-active set, so recording one here is
        // metadata-only. Zero is a deliberate sentinel — distinguishable
        // from the observations path which records the set that reached
        // quorum.
        0,
        digest_bump,
    )?;

    Ok(())
}

// ============================================================================
// Verify VAA Shim CPI — shape mirrored from `close_digest::verify_vaa`.
// ============================================================================

/// Verify the candidate digest via CPI to the Wormhole Verify VAA Shim
/// (`VerifyHash`). Real-CPI by default; replaced by a no-op under
/// `feature = "mock-vaa"` so the mollusk fast-path can drive `submit_vaas`
/// without standing up the Shim and its guardian-set fixtures.
///
/// The Shim's checks (see `programs/verify-vaa/src/lib.rs::process_verify_hash`):
///   1. `guardian_signatures` is owned by the Shim program.
///   2. `guardian_set`'s address matches `(GUARDIAN_SET_SEED,
///      guardian_index_be, guardian_set_bump)` under the Core Bridge program.
///   3. The guardian set is not expired.
///   4. The recovered Ethereum pubkeys reach quorum against the stored digest.
#[cfg(not(feature = "mock-vaa"))]
fn verify_vaa(
    verify_vaa_shim_program: &AccountView,
    guardian_set: &AccountView,
    guardian_signatures: &AccountView,
    digest: &[u8; 32],
    guardian_set_bump: u8,
) -> ProgramResult {
    // Defence-in-depth: refuse to CPI to anything other than the Shim. The
    // runtime would still reject a wrong program ID; failing here yields our
    // own error code in the program logs.
    if verify_vaa_shim_program.address().as_array()
        != &crate::definitions::VERIFY_VAA_SHIM_PROGRAM_ID
    {
        return Err(err(GlobalAccountantError::InvalidPda));
    }

    // Build the Shim's `VerifyHash` instruction data on the stack:
    //   [0..8]  = VERIFY_HASH_SELECTOR (Anchor discriminator)
    //   [8]     = guardian_set_bump
    //   [9..41] = digest
    let mut ix_data = [0u8; VERIFY_HASH_DATA_LEN];
    ix_data[..8].copy_from_slice(&VERIFY_HASH_SELECTOR);
    ix_data[8] = guardian_set_bump;
    ix_data[9..].copy_from_slice(digest);

    let ix_accounts = [
        InstructionAccount::readonly(guardian_set.address()),
        InstructionAccount::readonly(guardian_signatures.address()),
    ];

    let instruction = InstructionView {
        program_id: verify_vaa_shim_program.address(),
        data: &ix_data,
        accounts: &ix_accounts,
    };

    pinocchio::cpi::invoke(&instruction, &[guardian_set, guardian_signatures])
}

#[cfg(feature = "mock-vaa")]
fn verify_vaa(
    _verify_vaa_shim_program: &AccountView,
    _guardian_set: &AccountView,
    _guardian_signatures: &AccountView,
    _digest: &[u8; 32],
    _guardian_set_bump: u8,
) -> ProgramResult {
    Ok(())
}

// ============================================================================
// keccak256 helpers — same shape as `submit_observations` (the on-chain
// `sol_keccak256` ABI takes a pointer to `&[u8]` fat pointers, not raw bytes).
// ============================================================================

#[cfg(any(target_os = "solana", target_arch = "bpf"))]
fn keccak256(data: &[u8], result: &mut [u8; 32]) {
    let vals: [&[u8]; 1] = [data];
    // SAFETY: pinocchio re-exports the Solana syscall ABI; the runtime reads
    // exactly `val_len` `&[u8]` fat pointers starting at `vals_ptr`.
    unsafe {
        pinocchio::syscalls::sol_keccak256(
            vals.as_ptr() as *const u8,
            vals.len() as u64,
            result.as_mut_ptr(),
        );
    }
}

#[cfg(not(any(target_os = "solana", target_arch = "bpf")))]
fn keccak256(_data: &[u8], _result: &mut [u8; 32]) {}

/// `keccak256(keccak256(body))` — the Wormhole VAA digest convention.
fn double_keccak256(body: &[u8]) -> [u8; 32] {
    let mut inner = [0u8; 32];
    keccak256(body, &mut inner);
    let mut outer = [0u8; 32];
    keccak256(&inner, &mut outer);
    outer
}
