//! Product-neutral quorum primitives shared by every program's
//! `submit_observations` orchestration.
//!
//! `submit_observations` accumulates guardian signatures in a
//! `(chain, emitter, sequence, digest)`-keyed `PendingObservationsLayout` PDA.
//! The fields here are the building blocks each program's orchestration calls:
//! instruction-data parse, guardian signature verification against the Core
//! Bridge GuardianSet PDA, the pending-PDA lifecycle (create / wipe-recreate /
//! continue), the bitmap accumulation + quorum-threshold check, and the
//! rent-refunding close.
//!
//! WTT (`global-accountant`) and NTT (`ntt-global-accountant`) wire these into
//! distinct orchestrations with distinct account layouts and distinct
//! post-quorum balance flows; nothing here is product-specific.

use pinocchio::{
    cpi::{Seed, Signer},
    error::ProgramError,
    AccountView, Address, ProgramResult,
};

use crate::definitions::{
    parse_vaa_namespace_key, GlobalAccountantError, PendingObservationsLayout,
    CORE_BRIDGE_PROGRAM_ID, GUARDIAN_SET_SEED, PENDING_OBSERVATIONS_SEED_PREFIX,
};
use crate::err;
use crate::hash::keccak256;
use crate::instructions::pda_init::init_or_upgrade_pda;
use crate::state::pending;

/// Fixed-size prefix of `submit_observations` instruction data (after the
/// 1-byte dispatch discriminator):
///
/// | offset | size | field                              |
/// |--------|------|------------------------------------|
/// | 0      | 4    | guardian_set_index (little-endian) |
/// | 4      | 1    | guardian_index                     |
/// | 5      | 65   | signature (r||s||recovery_id)      |
///
/// Trailing the fixed prefix, each program's orchestration carries
/// `tx_hash: [u8; 32]`, then `body_len: u16 LE`, then `body_len` VAA body bytes.
/// Two digests are derived on-chain from these (never passed in):
///   - the *signing* digest `keccak256(prefix ‖ tx_hash ‖ body)` that the
///     guardian actually signed, checked by [`verify_signature`]; the
///     domain-separation `prefix` is product-specific (WTT vs NTT), and
///   - the *dedup/quorum* digest `keccak256(keccak256(body))` that keys the
///     pending PDA, the NoReplay slot, and the commit-log record.
///
/// PDA bumps are derived on-chain, not supplied.
///
/// The routing tuple `(chain, emitter, sequence)` is sourced exclusively from
/// the body header `[8..50]`, never caller-supplied data — otherwise an attacker
/// could replay a signed body under an arbitrary triple and corrupt the ledger.
pub const SUBMIT_FIXED_LEN: usize = 4 + 1 + 65;

/// ECDSA recoverable signature length: 32-byte r + 32-byte s + 1-byte recovery id.
pub const SECP256K1_SIGNATURE_LEN: usize = 65;

/// Ethereum-style guardian pubkey length (`keccak256(uncompressed_pk)[12..]`).
const GUARDIAN_PUBKEY_LEN: usize = 20;

/// `sol_secp256k1_recover` result buffer: 64-byte uncompressed pubkey (`X || Y`).
const SECP256K1_PUBKEY_RAW_LEN: usize = 64;

/// 51-byte VAA header + 1-byte action — the minimum body the parser can read.
pub const BODY_MIN_LEN: usize = 52;

/// The parsed `submit_observations` instruction-data prefix plus the routing
/// tuple recovered from the authenticated body header. Construct via
/// [`Self::from_data`], set `digest` to `keccak256(keccak256(body))`, then call
/// [`Self::populate_routing_from_body`].
#[derive(Clone, Copy)]
pub struct ParsedObservation {
    /// Signed digest, `keccak256(keccak256(body))`; derived from the body by the
    /// caller after `from_data`, not parsed from the instruction data.
    pub digest: [u8; 32],
    /// Body header `[8..10]`, populated by `populate_routing_from_body`.
    pub chain: u16,
    /// Body header `[10..42]`, populated by `populate_routing_from_body`.
    pub emitter: [u8; 32],
    /// Body header `[42..50]`, populated by `populate_routing_from_body`.
    pub sequence: u64,
    pub guardian_set_index: u32,
    pub guardian_index: u8,
    pub signature: [u8; SECP256K1_SIGNATURE_LEN],
}

impl ParsedObservation {
    /// Parse the signature fields from the fixed prefix. The digest and routing
    /// tuple are derived from the body afterward by the caller.
    pub fn from_data(data: &[u8; SUBMIT_FIXED_LEN]) -> Result<Self, ProgramError> {
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
    pub fn populate_routing_from_body(&mut self, body: &[u8]) -> Result<(), ProgramError> {
        let header = parse_vaa_namespace_key(body).map_err(err)?;
        self.chain = header.chain;
        self.emitter = header.emitter;
        self.sequence = header.sequence;
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PendingAction {
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
pub fn decide_pending_action(
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

/// Apply `pending_action`, then toggle this guardian's bit. Returns the loaded
/// layout (so the orchestration can read `payer` on close) and whether the
/// accumulated bit count has reached `quorum_threshold` — which the caller
/// derives from the live guardian-set size (see
/// `PendingObservationsLayout::quorum_for`), not a pinned constant. Idempotent
/// guards: a re-used guardian index rejects `AlreadySigned`; an out-of-range
/// index rejects `InvalidGuardianIndex`.
pub fn apply_action_and_accumulate(
    program_id: &Address,
    submitter: &mut AccountView,
    pending_pda: &mut AccountView,
    parsed: &ParsedObservation,
    action: PendingAction,
    quorum_threshold: u32,
) -> Result<(PendingObservationsLayout, bool), ProgramError> {
    match action {
        PendingAction::Create => {
            create_pending_pda(program_id, submitter, pending_pda, parsed)?;
        }
        PendingAction::WipeAndRecreate => {
            wipe_pending_pda(pending_pda, submitter)?;
            create_pending_pda(program_id, submitter, pending_pda, parsed)?;
        }
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

    let quorum_reached = layout.signatures.count_ones() >= quorum_threshold;
    Ok((layout, quorum_reached))
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
pub fn close_pending_pda(
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
/// Returns the live guardian count (`keys_len`) so the caller can derive the
/// quorum threshold from the actual set rather than a pinned constant.
///
/// SECURITY: the guardian keys are read straight out of `guardian_set`, which is
/// the *sole* authenticity anchor on the `submit_observations` path (unlike
/// `submit_vaas`, which delegates to the Verify VAA Shim). It MUST therefore be
/// the genuine Core Bridge GuardianSet account — otherwise a caller could pass a
/// forged account full of attacker-controlled pubkeys, sign the target digest
/// with the matching attacker keys, and self-accumulate to quorum, forging
/// arbitrary transfers. A Core-Bridge-owned account can only ever hold
/// Core-Bridge-written data, so asserting the owner is sufficient (and mirrors
/// `close_pending::guardian_set_expired`). The shim enforces the equivalent
/// constraint via Core-Bridge PDA-address derivation; see `shim::verify_vaa`.
pub fn verify_signature(
    guardian_set: &AccountView,
    expected_guardian_set_index: u32,
    guardian_index: u8,
    digest: &[u8; 32],
    signature: &[u8; SECP256K1_SIGNATURE_LEN],
) -> Result<u32, ProgramError> {
    // Verify the account is owned by Core Bridge.
    if guardian_set.owner().as_array() != &CORE_BRIDGE_PROGRAM_ID {
        return Err(err(GlobalAccountantError::InvalidPda));
    }

    // Verify the account is at the canonical GuardianSet PDA address. Owner alone
    // would suffice (a Core-Bridge-owned account can only hold Core-Bridge data),
    // but pinning the address rejects a stale/wrong-index set up front.
    let index_be = expected_guardian_set_index.to_be_bytes();
    let core_bridge_addr = Address::from(CORE_BRIDGE_PROGRAM_ID);
    let (expected_address, _) =
        Address::find_program_address(&[GUARDIAN_SET_SEED, &index_be], &core_bridge_addr);
    if guardian_set.address() != &expected_address {
        return Err(err(GlobalAccountantError::InvalidPda));
    }

    let data = guardian_set.try_borrow()?;
    let expected_key = read_guardian_key(&data, expected_guardian_set_index, guardian_index)?;
    // `read_guardian_key` already proved `data.len() >= 8`, so `keys_len` at
    // `[4..8]` is in bounds. This is the live set size the quorum derives from.
    let num_guardians = u32::from_le_bytes([data[4], data[5], data[6], data[7]]);

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
    Ok(num_guardians)
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
pub fn read_guardian_key(
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
