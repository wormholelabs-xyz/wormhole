//! `submit_observations` — quorum tracker.
//!
//! A `(chain, emitter, sequence, digest)`-keyed `PendingObservationsLayout` PDA
//! accumulates guardian signatures. The quorum-completing observation atomically
//! flips the NoReplay slot, emits the canonical digest record via
//! `commit_log::emit`, applies balance effects, and closes the pending PDA,
//! refunding rent to its recorded payer.
//!
//! Sibling buckets at the same `(chain, emitter, sequence)` but different digests
//! (source-chain reorg) race independently; losers are reclaimed via
//! `close_pending`. Signatures are verified inline via the `secp256k1_recover`
//! syscall; only the bitmap is persisted.

use pinocchio::{
    cpi::{Seed, Signer},
    error::ProgramError,
    AccountView, Address, ProgramResult,
};

use crate::definitions::{
    parse_token_bridge_payload, parse_vaa_namespace_key, GlobalAccountantError,
    PendingObservationsLayout, TokenBridgeAction, CORE_BRIDGE_PROGRAM_ID, GUARDIAN_SET_SEED,
    PENDING_OBSERVATIONS_SEED_PREFIX, VAA_BODY_HEADER_LEN,
};
use crate::err;
use crate::hash::{double_keccak256, keccak256};
use crate::instructions::{
    commit_log, noreplay, pda_init::init_or_upgrade_pda, transfer::apply_transfer,
};
use crate::state::{chain_registration, pending};

/// Fixed-size prefix of `submit_observations` instruction data (after the
/// 1-byte dispatch discriminator):
///
/// | offset | size | field                              |
/// |--------|------|------------------------------------|
/// | 0      | 4    | guardian_set_index (little-endian) |
/// | 4      | 1    | guardian_index                     |
/// | 5      | 65   | signature (r||s||recovery_id)      |
///
/// Trailing the prefix is `body_len: u16 LE` then `body_len` VAA body bytes.
/// The signed digest is `keccak256(keccak256(body))`, derived on-chain from the
/// supplied body rather than passed in: the recovered signature is checked
/// against it, so the body authenticates itself. PDA bumps are derived on-chain,
/// not supplied.
///
/// The routing tuple `(chain, emitter, sequence)` is sourced exclusively from
/// the body header `[8..50]`, never caller-supplied data — otherwise an attacker
/// could replay a signed body under an arbitrary triple and corrupt the ledger.
const SUBMIT_FIXED_LEN: usize = 4 + 1 + SECP256K1_SIGNATURE_LEN;

/// ECDSA recoverable signature length: 32-byte r + 32-byte s + 1-byte recovery id.
const SECP256K1_SIGNATURE_LEN: usize = 32 + 32 + 1;

/// Ethereum-style guardian pubkey length (`keccak256(uncompressed_pk)[12..]`).
const GUARDIAN_PUBKEY_LEN: usize = 20;

/// `sol_secp256k1_recover` result buffer: 64-byte uncompressed pubkey (`X || Y`).
const SECP256K1_PUBKEY_RAW_LEN: usize = 32 + 32;

/// Minimum VAA body length: the fixed body header plus a 1-byte action.
const BODY_MIN_LEN: usize = VAA_BODY_HEADER_LEN + 1;

pub fn process(program_id: &Address, accounts: &mut [AccountView], data: &[u8]) -> ProgramResult {
    // Split into fixed prefix + length-prefixed body. The body is required: the
    // signed digest and the routing tuple are both derived from it.
    if data.len() < SUBMIT_FIXED_LEN + 2 {
        return Err(err(GlobalAccountantError::InvalidInstructionData));
    }
    let (fixed_bytes, rest) = data.split_at(SUBMIT_FIXED_LEN);
    let fixed_bytes: &[u8; SUBMIT_FIXED_LEN] = fixed_bytes
        .try_into()
        .map_err(|_| err(GlobalAccountantError::InvalidInstructionData))?;
    let body_len = u16::from_le_bytes([rest[0], rest[1]]) as usize;
    if body_len < BODY_MIN_LEN || rest.len() < 2 + body_len {
        return Err(err(GlobalAccountantError::InvalidInstructionData));
    }
    let body_bytes = &rest[2..2 + body_len];

    let mut parsed = ParsedObservation::from_data(fixed_bytes)?;

    // Derive the signed digest from the supplied body. The signature is verified
    // against this value below, so the body authenticates itself.
    parsed.digest = double_keccak256(body_bytes);

    // Routing tuple is sourced from the now-authenticated body header, never
    // caller-supplied data — see the module-level wire-format doc.
    parsed.populate_routing_from_body(body_bytes)?;

    // Accounts:
    //   0. `[WRITE, SIGNER]` submitter (fee + rent payer for all lazy PDAs).
    //   1. `[WRITE]`         pending PDA.
    //   2. `[]`              GuardianSet PDA (Core Bridge).
    //   3. `[WRITE]`         NoReplay bitmap PDA (read at pre-check, written at
    //                       commit; always WRITE per runtime declaration rules).
    //   4. `[]`              system program.
    //   5. `[]`              NoReplay program (CPI target).
    //   6. `[]`              NoReplay authority PDA owned by this program.
    //   7. `[WRITE]`         source-chain balance account PDA. Only touched on the
    //                       quorum-completing Transfer branch; sentinel otherwise.
    //   8. `[WRITE]`         destination-chain balance account PDA. Same semantics as slot 7.
    //   9. `[WRITE]`         rent recipient for the pending PDA close. Must equal
    //                       the bucket's recorded payer (rejected as `PayerMismatch`
    //                       otherwise); decoupled from submitter so any guardian
    //                       can complete quorum on the opener's behalf.
    //  10. `[]`              chain registration PDA. Read to verify the body's
    //                       `(emitter_chain, emitter_address)` is a registered
    //                       emitter; system-owned ⇒ `MissingChainRegistration`.
    //
    // The canonical digest record is emitted via `sol_log_data` rather than
    // stored in a PDA; off-chain indexers consume the program-log line carrying
    // the `ACCOUNTANT_DIGEST_LOG_TAG` prefix.
    let [submitter, pending_pda, guardian_set, noreplay_bucket, system_program_acc, noreplay_program, noreplay_authority, source_account_pda, dest_account_pda, rent_recipient, chain_registration_pda] =
        accounts
    else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    if !submitter.is_signer() {
        return Err(ProgramError::MissingRequiredSignature);
    }

    // NoReplay pre-check rejects replays before any signature work.
    if noreplay::is_marked(
        noreplay_bucket,
        noreplay_authority.address(),
        parsed.chain,
        &parsed.emitter,
        parsed.sequence,
    )? {
        return Err(err(GlobalAccountantError::AlreadyAccounted));
    }

    // Chain-registration cross-check: reject valid sigs for an unregistered
    // (and therefore potentially fake) emitter. PDA address verified first.
    chain_registration::verify(
        program_id,
        chain_registration_pda,
        parsed.chain,
        &parsed.emitter,
    )?;

    verify_signature(
        guardian_set,
        parsed.guardian_set_index,
        parsed.guardian_index,
        &parsed.digest,
        &parsed.signature,
    )?;

    let pending_action = decide_pending_action(program_id, pending_pda, &parsed)?;

    match pending_action {
        // 1st observation out of 13
        PendingAction::Create => {
            create_pending_pda(program_id, submitter, pending_pda, &parsed)?;
        }
        // 1st observation and its dusted
        PendingAction::WipeAndRecreate => {
            wipe_pending_pda(pending_pda, submitter)?;
            create_pending_pda(program_id, submitter, pending_pda, &parsed)?;
        }
        // One of the observations between 2-13
        PendingAction::Continue => {}
    }

    let mut layout = pending::load(pending_pda)?;
    let bit = 1u32
        .checked_shl(parsed.guardian_index as u32)
        .ok_or_else(|| err(GlobalAccountantError::InvalidGuardianIndex))?;
    if layout.signatures & bit != 0 {
        return Err(err(GlobalAccountantError::AlreadySigned));
    }
    layout.signatures |= bit;
    pending::store(pending_pda, &layout)?;

    let popcount = layout.signatures.count_ones();
    if popcount < PendingObservationsLayout::QUORUM_THRESHOLD {
        return Ok(());
    }

    // Quorum reached. Commit atomically: NoReplay flip, canonical log emit,
    // balance accounting, pending close (tx-level rollback covers failures).
    noreplay::mark_used(
        submitter,
        noreplay_bucket,
        noreplay_program,
        noreplay_authority,
        system_program_acc,
        program_id,
        parsed.chain,
        &parsed.emitter,
        parsed.sequence,
    )?;

    commit_log::emit(
        parsed.chain,
        &parsed.emitter,
        parsed.sequence,
        &parsed.digest,
        parsed.guardian_set_index,
    );

    // Transfer payloads mutate two balance account PDAs; Attest skips balance work.
    match parse_token_bridge_payload(body_bytes).map_err(err)? {
        TokenBridgeAction::Transfer {
            amount,
            token_chain,
            token_address,
            recipient_chain,
        } => {
            // Source chain is the authenticated VAA emitter chain.
            let source_chain = parsed.chain;
            apply_transfer(
                program_id,
                submitter,
                source_account_pda,
                dest_account_pda,
                source_chain,
                recipient_chain,
                token_chain,
                &token_address,
                amount,
            )?;
        }
        TokenBridgeAction::Attest => {
            // No balance work; slots 8 and 9 are untouched (sentinels OK).
        }
        TokenBridgeAction::Other => {
            // Unknown action: reject. The NoReplay mark rolls back with the tx,
            // leaving the slot unconsumed for a future upgrade.
            return Err(err(GlobalAccountantError::UnknownTokenBridgePayload));
        }
    }

    let recorded_payer = layout.payer;
    close_pending_pda(pending_pda, rent_recipient, &recorded_payer)?;
    Ok(())
}

#[derive(Clone, Copy)]
struct ParsedObservation {
    /// Signed digest, `keccak256(keccak256(body))`; derived from the body in
    /// `process`, not parsed from the instruction data.
    digest: [u8; 32],
    /// Body header `[8..10]`, populated by `populate_routing_from_body`.
    chain: u16,
    /// Body header `[10..42]`, populated by `populate_routing_from_body`.
    emitter: [u8; 32],
    /// Body header `[42..50]`, populated by `populate_routing_from_body`.
    sequence: u64,
    guardian_set_index: u32,
    guardian_index: u8,
    signature: [u8; SECP256K1_SIGNATURE_LEN],
}

impl ParsedObservation {
    /// Parse the signature fields from the fixed prefix. The digest and routing
    /// tuple are derived from the body afterward in `process`.
    fn from_data(data: &[u8; SUBMIT_FIXED_LEN]) -> Result<Self, ProgramError> {
        let (gsi_bytes, rest) = data.split_at(4);
        let guardian_index = rest[0];
        let signature_bytes = &rest[1..1 + SECP256K1_SIGNATURE_LEN];

        let gsi: [u8; 4] = gsi_bytes
            .try_into()
            .map_err(|_| err(GlobalAccountantError::InvalidInstructionData))?;
        let signature: [u8; SECP256K1_SIGNATURE_LEN] = signature_bytes
            .try_into()
            .map_err(|_| err(GlobalAccountantError::InvalidInstructionData))?;

        Ok(Self {
            digest: [0u8; 32],
            chain: 0,
            emitter: [0u8; 32],
            sequence: 0,
            guardian_set_index: u32::from_le_bytes(gsi),
            guardian_index,
            signature,
        })
    }

    /// Populate the routing tuple from the body header. `self.digest` must have
    /// been derived from this same `body`.
    fn populate_routing_from_body(&mut self, body: &[u8]) -> Result<(), ProgramError> {
        let header = parse_vaa_namespace_key(body).map_err(err)?;
        self.chain = header.chain;
        self.emitter = header.emitter;
        self.sequence = header.sequence;
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PendingAction {
    /// PDA does not exist yet — allocate, assign, write a fresh layout.
    Create,
    /// PDA exists for an older guardian set — refund payer, wipe, re-create.
    WipeAndRecreate,
    /// PDA exists for the same guardian set — toggle the bitmap bit.
    Continue,
}

/// Verify a pending PDA lives at its canonical address derived from
/// `(chain, emitter, sequence, digest)`. Follows the same pattern as
/// `chain_registration::verify`.
fn verify_pending_pda_address(
    program_id: &Address,
    pending_pda: &AccountView,
    chain: u16,
    emitter: &[u8; 32],
    sequence: u64,
    digest: &[u8; 32],
) -> ProgramResult {
    let chain_be = chain.to_be_bytes();
    let sequence_be = sequence.to_be_bytes();
    let (expected, _bump) = Address::find_program_address(
        &[
            PENDING_OBSERVATIONS_SEED_PREFIX,
            &chain_be,
            emitter,
            &sequence_be,
            digest,
        ],
        program_id,
    );
    if pending_pda.address() != &expected {
        return Err(err(GlobalAccountantError::InvalidPda));
    }
    Ok(())
}

/// Decide what to do with the pending PDA for this observation. Per-digest PDA
/// seeds and canonical address verification ensure a digest mismatch is
/// unreachable: any loaded PDA was opened under exactly this digest.
fn decide_pending_action(
    program_id: &Address,
    pending_pda: &AccountView,
    parsed: &ParsedObservation,
) -> Result<PendingAction, ProgramError> {
    let owner_is_system = pending_pda.owner() == &pinocchio_system::ID;
    let data_len = pending_pda.data_len();

    if owner_is_system && data_len == 0 {
        return Ok(PendingAction::Create);
    }
    if owner_is_system {
        // System-owned with non-zero data is unreachable on Solana; reject loudly.
        return Err(err(GlobalAccountantError::InvalidPda));
    }

    // Non-system owner: must be us. Verify the canonical address first.
    verify_pending_pda_address(
        program_id,
        pending_pda,
        parsed.chain,
        &parsed.emitter,
        parsed.sequence,
        &parsed.digest,
    )?;

    // Load and compare guardian set indices.
    let existing = pending::load(pending_pda)?;
    if existing.guardian_set_index < parsed.guardian_set_index {
        return Ok(PendingAction::WipeAndRecreate);
    }
    if existing.guardian_set_index > parsed.guardian_set_index {
        return Err(err(GlobalAccountantError::StaleGuardianSet));
    }
    // Digest equality is guaranteed by the per-digest PDA seeds AND canonical address.
    Ok(PendingAction::Continue)
}

/// Allocate the pending PDA under `(b"pending", chain, emitter, sequence,
/// digest)` and stamp a freshly-zeroed layout. The digest in the seeds lets
/// reorg siblings accumulate in parallel buckets.
fn create_pending_pda(
    program_id: &Address,
    submitter: &AccountView,
    pending_pda: &mut AccountView,
    parsed: &ParsedObservation,
) -> ProgramResult {
    // Canonical bump derived on-chain; `invoke_signed` below only signs for the
    // canonical address, so a non-canonical sibling PDA is impossible.
    let chain_be = parsed.chain.to_be_bytes();
    let sequence_be = parsed.sequence.to_be_bytes();
    let (_expected, canonical_bump) = Address::find_program_address(
        &[
            PENDING_OBSERVATIONS_SEED_PREFIX,
            &chain_be,
            &parsed.emitter,
            &sequence_be,
            &parsed.digest,
        ],
        program_id,
    );

    let bump_seed = [canonical_bump];
    let seeds = [
        Seed::from(PENDING_OBSERVATIONS_SEED_PREFIX),
        Seed::from(chain_be.as_slice()),
        Seed::from(parsed.emitter.as_slice()),
        Seed::from(sequence_be.as_slice()),
        Seed::from(parsed.digest.as_slice()),
        Seed::from(bump_seed.as_slice()),
    ];
    let signer = Signer::from(&seeds);

    init_or_upgrade_pda(
        submitter,
        pending_pda,
        program_id,
        signer,
        PendingObservationsLayout::LEN as u64,
    )?;

    let mut layout: PendingObservationsLayout = bytemuck::Zeroable::zeroed();
    layout.tag = PendingObservationsLayout::TAG;
    layout.digest = parsed.digest;
    layout.payer = *submitter.address().as_array();
    layout.guardian_set_index = parsed.guardian_set_index;
    layout.signatures = 0;
    layout.chain = parsed.chain;
    pending::store(pending_pda, &layout)
}

/// Refund the recorded payer and close the account. `recorded_payer` is passed
/// in to avoid re-borrowing the already-loaded layout.
pub(crate) fn close_pending_pda(
    pending_pda: &mut AccountView,
    rent_recipient: &mut AccountView,
    recorded_payer: &[u8; 32],
) -> ProgramResult {
    if rent_recipient.address().as_array() != recorded_payer {
        return Err(err(GlobalAccountantError::PayerMismatch));
    }
    let lamports = pending_pda.lamports();
    let recipient_lamports = rent_recipient.lamports();
    rent_recipient.set_lamports(
        recipient_lamports
            .checked_add(lamports)
            .ok_or(ProgramError::ArithmeticOverflow)?,
    );
    pending_pda.close()
}

/// Rotation-wipe variant: credits the PDA's lamports to the new submitter
/// rather than the recorded payer. The wire shape carries no original-payer
/// account on rotation, so that payer's (bounded, ~$0.10) rent is forfeit to
/// whoever pays the rotation cost; `close_pending` remains available to recover
/// it ahead of rotation.
fn wipe_pending_pda(
    pending_pda: &mut AccountView,
    new_submitter: &mut AccountView,
) -> ProgramResult {
    let lamports = pending_pda.lamports();
    let submitter_lamports = new_submitter.lamports();
    new_submitter.set_lamports(
        submitter_lamports
            .checked_add(lamports)
            .ok_or(ProgramError::ArithmeticOverflow)?,
    );
    pending_pda.close()
}

/// Verify a guardian signature: recover the pubkey via `secp256k1_recover` and
/// compare its keccak hash to the key in the Core Bridge GuardianSet PDA.
fn verify_signature(
    guardian_set: &AccountView,
    expected_guardian_set_index: u32,
    guardian_index: u8,
    digest: &[u8; 32],
    signature: &[u8; SECP256K1_SIGNATURE_LEN],
) -> ProgramResult {
    // Verify the account is owned by Core Bridge.
    if guardian_set.owner().as_array() != &CORE_BRIDGE_PROGRAM_ID {
        return Err(err(GlobalAccountantError::InvalidPda));
    }

    // Verify the account is at the canonical Guardian Set PDA address.
    let index_be = expected_guardian_set_index.to_be_bytes();
    let core_bridge_addr = Address::from(CORE_BRIDGE_PROGRAM_ID);
    let (expected_address, _) =
        Address::find_program_address(&[GUARDIAN_SET_SEED, &index_be], &core_bridge_addr);
    if guardian_set.address() != &expected_address {
        return Err(err(GlobalAccountantError::InvalidPda));
    }

    let data = guardian_set.try_borrow()?;
    let expected_key = read_guardian_key(&data, expected_guardian_set_index, guardian_index)?;

    // Recovery id ∈ {0,1,2,3}; values >= 4 are malformed.
    let recovery_id = signature[64];
    if recovery_id >= 4 {
        return Err(err(GlobalAccountantError::InvalidSignature));
    }

    let mut recovered = [0u8; SECP256K1_PUBKEY_RAW_LEN];
    let rc = secp256k1_recover(digest, recovery_id as u64, &signature[..64], &mut recovered);
    if rc != 0 {
        return Err(err(GlobalAccountantError::InvalidSignature));
    }

    // Compare `keccak256(recovered_pk)[12..]` to the stored guardian key.
    let mut hash = [0u8; 32];
    keccak256(&recovered, &mut hash);
    if hash[12..] != expected_key[..] {
        return Err(err(GlobalAccountantError::InvalidSignature));
    }
    Ok(())
}

/// Read the 20-byte guardian pubkey at `guardian_index` from a Core Bridge
/// `GuardianSet` account.
///
/// On-disk layout:
///
/// | offset | size | field              |
/// |--------|------|--------------------|
/// | 0      | 4    | guardian_set_index |
/// | 4      | 4    | keys_len           |
/// | 8      | 20*N | keys               |
/// | 8+20N  | 4    | creation_time      |
/// | 12+20N | 4    | expiration_time    |
fn read_guardian_key(
    data: &[u8],
    expected_index: u32,
    guardian_index: u8,
) -> Result<[u8; GUARDIAN_PUBKEY_LEN], ProgramError> {
    if data.len() < 8 {
        return Err(ProgramError::InvalidAccountData);
    }
    // `data.len() >= 8` is guaranteed above, so these reads are infallible.
    let on_chain_index = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
    if on_chain_index != expected_index {
        return Err(err(GlobalAccountantError::InvalidGuardianIndex));
    }
    let keys_len = u32::from_le_bytes([data[4], data[5], data[6], data[7]]);
    if (guardian_index as u32) >= keys_len {
        return Err(err(GlobalAccountantError::InvalidGuardianIndex));
    }
    let start = 8 + (guardian_index as usize) * GUARDIAN_PUBKEY_LEN;
    let end = start + GUARDIAN_PUBKEY_LEN;
    if data.len() < end {
        return Err(ProgramError::InvalidAccountData);
    }
    let mut key = [0u8; GUARDIAN_PUBKEY_LEN];
    key.copy_from_slice(&data[start..end]);
    Ok(key)
}

// SBF uses the syscall; the host arm is a build-only stub (returns 1) so the
// crate compiles under `cargo check` outside `cargo build-sbf`.
fn secp256k1_recover(
    hash: &[u8; 32],
    recovery_id: u64,
    signature: &[u8],
    result: &mut [u8],
) -> u64 {
    // SAFETY: buffers match the syscall ABI: 32-byte hash, 64-byte signature
    // (`r||s`), 64-byte result.
    #[cfg(any(target_os = "solana", target_arch = "bpf"))]
    let code = unsafe {
        pinocchio::syscalls::sol_secp256k1_recover(
            hash.as_ptr(),
            recovery_id,
            signature.as_ptr(),
            result.as_mut_ptr(),
        )
    };
    #[cfg(not(any(target_os = "solana", target_arch = "bpf")))]
    let code = {
        let _ = (hash, recovery_id, signature, result);
        1
    };
    code
}
