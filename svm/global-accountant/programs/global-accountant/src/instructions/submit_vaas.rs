//! `submit_vaas` — permissionless signed-VAA backfill.
//!
//! Port of CosmWasm's `handle_tokenbridge_vaa`
//! (`cosmwasm/contracts/global-accountant/src/contract.rs:442-495`).
//! Consumes a fully-signed VAA via the Verify VAA Shim CPI and applies its
//! balance effects directly, bypassing the per-`(chain, emitter, sequence,
//! digest)` quorum tracker. The escape hatch for stuck pending buckets and
//! the unblock path for migration backfill.
//!
//! Orthogonality with `submit_observations`: NoReplay is the only shared
//! state, and both paths set it the same way. Once NoReplay marks
//! `(chain, emitter, seq)`, any subsequent caller — observation or VAA —
//! is rejected as `AlreadyAccounted` (CosmWasm's `DIGESTS DuplicateMessage`
//! short-circuit; the digest-mismatch sub-case is subsumed because the
//! namespace omits digest).
//!
//! Attest payloads (action 0x02) no-op the balance work but still commit the
//! DigestAccount + flip NoReplay. Unknown action bytes are rejected with
//! `UnknownTokenBridgePayload` (mirroring CosmWasm's bail), leaving the
//! NoReplay slot unconsumed so a future upgrade that understands the action
//! can still process the VAA.

use pinocchio::{
    error::ProgramError,
    instruction::{InstructionAccount, InstructionView},
    AccountView, Address, ProgramResult,
};

use crate::definitions::{
    parse_token_bridge_payload, parse_vaa_body_header, GlobalAccountantError, TokenBridgeAction,
    VAA_BODY_HEADER_LEN, VERIFY_HASH_DATA_LEN, VERIFY_HASH_SELECTOR,
};
use crate::err;
use crate::instructions::{noreplay, open_digest_inner, transfer::apply_transfer};
use crate::state::chain_registration;

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
///
/// No upper bound is imposed on `body_len` beyond the `u16` wire width —
/// the transports themselves (1232-byte tx packets, 10 KiB CPI instruction
/// data) are far tighter, and the CosmWasm accountant baseline imposes no
/// cap. See the matching rationale in `submit_observations`.
const SUBMIT_VAAS_FIXED_LEN: usize = 1 + 2;

pub fn process(program_id: &Address, accounts: &mut [AccountView], data: &[u8]) -> ProgramResult {
    // ----- (1) Parse wire data -----
    if data.len() < SUBMIT_VAAS_FIXED_LEN {
        return Err(err(GlobalAccountantError::InvalidInstructionData));
    }
    let guardian_set_bump = data[0];
    let body_len = u16::from_le_bytes([data[1], data[2]]) as usize;
    if body_len <= VAA_BODY_HEADER_LEN || data.len() != SUBMIT_VAAS_FIXED_LEN + body_len {
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
    //  11. `[]`              Chain registration PDA at
    //                       `(b"chain_registration", body_chain.to_be_bytes())`.
    //                       Populated by the `register_chain` governance
    //                       instruction. Read here to cross-check the body
    //                       header's `(emitter_chain, emitter_address)`
    //                       against a Token-Bridge-governance-registered
    //                       emitter. Mirrors the same check on the
    //                       `submit_observations` path and CosmWasm
    //                       `handle_tokenbridge_vaa` at
    //                       `contract.rs:446-454`.
    let [submitter, verify_vaa_shim_program, guardian_set, guardian_signatures, digest_pda, noreplay_bucket, noreplay_program, noreplay_authority, source_account_pda, dest_account_pda, _system_program, chain_registration_pda] =
        accounts
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
    // Shared with `submit_observations` via `definitions::parse_vaa_body_header`
    // — the single authority for the header offsets, so the two paths can
    // never route the same VAA to different `(chain, emitter, sequence)` keys.
    let header = parse_vaa_body_header(body_bytes).map_err(err)?;
    let (chain, emitter, sequence) = (header.chain, header.emitter, header.sequence);

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

    // ----- (5b) Chain registration cross-check -----
    //
    // Mirrors CosmWasm `handle_tokenbridge_vaa` at `contract.rs:446-454`. The
    // body's (emitter_chain, emitter_address) pair must correspond to a Token
    // Bridge emitter previously registered via the `register_chain`
    // governance instruction; otherwise an attacker with valid guardian sigs
    // for some non-Token-Bridge VAA could route accounting against a fake
    // emitter on a real chain.
    chain_registration::verify(program_id, chain_registration_pda, chain, &emitter)?;

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
        TokenBridgeAction::Attest => {
            // No balance work; the commit below still runs. Same sentinel-slot
            // convention as `submit_observations` — callers may pass the
            // noreplay-authority PDA in slots 8 and 9.
        }
        TokenBridgeAction::Other => {
            // Unknown action byte: reject BEFORE the NoReplay mark, mirroring
            // CosmWasm's `bail!("Unknown tokenbridge payload")`. Committing
            // here would burn the `(chain, emitter, sequence)` slot for a
            // payload this build cannot account, making the VAA permanently
            // unprocessable even after an upgrade that understands it.
            return Err(err(GlobalAccountantError::UnknownTokenBridgePayload));
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
    // slot, same as the observations path. The canonical digest-PDA bump is
    // derived inside `open_digest_inner`.
    open_digest_inner(
        program_id,
        submitter,
        digest_pda,
        chain.to_be_bytes(),
        emitter,
        sequence.to_be_bytes(),
        digest,
        // `submit_vaas` does not track a guardian-set index. The Shim
        // accepts any currently-active set, so recording one here is
        // metadata-only. Zero is a deliberate sentinel — distinguishable
        // from the observations path which records the set that reached
        // quorum.
        0,
    )?;

    Ok(())
}

// ============================================================================
// Verify VAA Shim CPI — shape mirrored from `close_digest::verify_vaa`.
// ============================================================================

/// Verify the candidate digest via CPI to the Wormhole Verify VAA Shim
/// (`VerifyHash`).
///
/// The Shim's checks (see `programs/verify-vaa/src/lib.rs::process_verify_hash`):
///   1. `guardian_signatures` is owned by the Shim program.
///   2. `guardian_set`'s address matches `(GUARDIAN_SET_SEED,
///      guardian_index_be, guardian_set_bump)` under the Core Bridge program.
///   3. The guardian set is not expired.
///   4. The recovered Ethereum pubkeys reach quorum against the stored digest.
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

use crate::hash::double_keccak256;
