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

use pinocchio::{error::ProgramError, AccountView, Address, ProgramResult};

use crate::definitions::{
    parse_token_bridge_payload, parse_vaa_body_header, GlobalAccountantError, TokenBridgeAction,
    VAA_BODY_HEADER_LEN,
};
use crate::err;
use crate::instructions::{
    noreplay, open_digest::open_digest_inner, shim, transfer::apply_transfer,
};
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
    let [submitter, _verify_vaa_shim_program, guardian_set, guardian_signatures, digest_pda, noreplay_bucket, noreplay_program, noreplay_authority, source_account_pda, dest_account_pda, _system_program, chain_registration_pda] =
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
    shim::verify_vaa(
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

    // ----- (6) NoReplay mark-used CPI -----
    //
    // Burn the `(chain, emitter, seq)` slot BEFORE any accountant state
    // change. Ethos: stop replays first, mutate balances last. Mirrors
    // `submit_observations`' commit-branch ordering. Tx-level atomicity
    // covers the failure path — any subsequent error (`apply_transfer`
    // overflow/underflow, `UnknownTokenBridgePayload`) rolls back the mark
    // with everything else, leaving the slot unconsumed.
    //
    // CosmWasm `handle_vaa` cross-checks `DIGESTS[(chain, emitter, seq)]`
    // and short-circuits on a match (`DuplicateMessage` — see
    // `contract.rs:324-333`). The Solana port collapses the digest-table
    // check into NoReplay.
    //
    // A racing tx that flipped the same bit between our pre-check and this
    // CPI surfaces as `NoReplayCpiFailed`, which is the runtime's escape
    // valve.
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

    // ----- (7) Open the DigestAccount PDA -----
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

    // ----- (8) Parse Token Bridge payload + apply balance work -----
    //
    // Last step in the commit branch, mirroring `submit_observations`. The
    // payload mutation runs *after* the replay slot is claimed and the
    // DigestAccount breadcrumb is laid down — the NoReplay mark is the
    // gate, the balance mutation is the consequence.
    //
    // CosmWasm `handle_tokenbridge_vaa` parses the payload as
    // `wormhole_sdk::token::Message::{Transfer, TransferWithPayload}` and
    // calls `accountant::commit_transfer`. The Solana port routes through
    // the same helper as `submit_observations`' quorum-completing branch —
    // shared via `instructions::transfer::apply_transfer`.
    //
    // Attest payloads: no balance work, but the prior NoReplay flip +
    // DigestAccount open still commit on the success path. Matches
    // CosmWasm's behaviour for any payload other than 0x01 / 0x03.
    //
    // Unknown action bytes: reject. Tx-level atomicity rolls back the
    // NoReplay mark and the DigestAccount open, leaving the slot unconsumed
    // so a future upgrade that understands the action can still process the
    // VAA. (If we later decide that overflow/underflow should *consume* the
    // slot — a VAA that authenticated past the Shim but cannot be accounted
    // would then still count as "seen" — the policy lives here.)
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
            // No balance work. Same sentinel-slot convention as
            // `submit_observations` — callers may pass the noreplay-authority
            // PDA in slots 8 and 9.
        }
        TokenBridgeAction::Other => {
            return Err(err(GlobalAccountantError::UnknownTokenBridgePayload));
        }
    }

    Ok(())
}

use crate::hash::double_keccak256;
