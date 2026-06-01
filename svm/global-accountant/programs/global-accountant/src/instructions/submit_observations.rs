//! `submit_observations` — quorum tracker.
//!
//! A `(chain, emitter, sequence, digest)`-keyed `PendingObservationsLayout`
//! PDA accumulates guardian signatures; the 13th observation in any one
//! bucket atomically
//!
//! 1. flips the NoReplay slot (shared across sibling buckets at the same
//!    `(chain, emitter, sequence)`),
//! 2. opens the `DigestAccount` PDA via `super::open_digest_inner`, and
//! 3. closes the winning pending PDA, refunding rent to its recorded payer.
//!
//! Sibling buckets at the same `(chain, emitter, sequence)` but different
//! digests (the source-chain reorg case) coexist and race independently; the
//! losing buckets are reclaimed via `close_pending`'s NoReplay-marked
//! trigger.
//!
//! Signature verification is inline via the Solana-native `secp256k1_recover`
//! syscall (`pinocchio::syscalls::sol_secp256k1_recover`). No raw signatures
//! are persisted — only the popcount-counted bitmap survives in the pending
//! PDA, matching the CosmWasm baseline.
//!
//! NoReplay integration is gated behind the `mock-noreplay` Cargo feature
//! mirroring `mock-vaa`'s shape (`noreplay::is_marked` / `noreplay::mark_used`
//! below).

use pinocchio::{
    cpi::{Seed, Signer},
    error::ProgramError,
    sysvars::{clock::Clock, Sysvar},
    AccountView, Address, ProgramResult,
};

use crate::definitions::{
    parse_token_bridge_payload, GlobalAccountantError, PendingObservationsLayout,
    TokenBridgeAction, PENDING_SEED_PREFIX,
};
use crate::err;
// NoReplay integration lives in the sibling `noreplay` module so
// `close_pending` can re-use `is_marked` for its trigger-(b) check without
// re-importing this module's private items. The balance-mutation helper
// (`apply_transfer`) lives in a sibling `transfer` module so `submit_vaas`
// can re-use it without depending on this module's internals.
use crate::instructions::{
    noreplay, open_digest_inner, pda_init::init_or_upgrade_pda, transfer::apply_transfer,
};
use crate::state::{chain_registration, pending};

/// Wire format for the fixed-size portion of `submit_observations`
/// instruction data (after the 1-byte dispatch discriminator):
///
/// | offset | size | field                              |
/// |--------|------|------------------------------------|
/// | 0      | 32   | digest                             |
/// | 32     | 4    | guardian_set_index (little-endian) |
/// | 36     | 1    | guardian_index                     |
/// | 37     | 65   | signature (r||s||recovery_id)      |
/// | 102    | 1    | pending_pda_bump                   |
/// | 103    | 1    | digest_pda_bump                    |
///
/// Trailing the fixed-size portion is `body_len: u16 LE` followed by exactly
/// `body_len` bytes of VAA body. The body is verified against the supplied
/// `digest` via `keccak256(keccak256(body)) == digest` before any state
/// mutation, then parsed for Token Bridge fields on the quorum-completing
/// branch.
///
/// The routing tuple `(chain, emitter, sequence)` is sourced exclusively from
/// the body's own header at byte offsets `[8..50]`, matching the CosmWasm
/// `Observation::digest` precedent in `cosmwasm/packages/accountant/src/msg.rs`
/// where the bucket key and the digest preimage are the same bytes. Carrying
/// a separate caller-controlled prefix would let an attacker replay a signed
/// body under arbitrary `(chain, emitter, sequence)` triples, corrupting the
/// balance ledger; sourcing them from the body makes that attack structurally
/// impossible.
const SUBMIT_FIXED_LEN: usize = 32 + 4 + 1 + 65 + 1 + 1;
/// Maximum supported VAA body size on the wire. 4 KiB is well above the
/// observed mainnet ceiling (`max(payload) ≈ 1 KiB`) and keeps the
/// instruction data within Solana's 1232-byte tx-data limit when combined
/// with the fixed-size prefix and the account list. The bound exists only to
/// reject malformed wire data early; the parser itself doesn't care.
const SUBMIT_BODY_MAX: usize = 4096;

/// Length of an ECDSA recoverable signature: 32-byte r + 32-byte s + 1-byte
/// recovery id. The on-chain `sol_secp256k1_recover` syscall takes the 64-byte
/// `r||s` prefix and the recovery id separately.
const SECP256K1_SIGNATURE_LEN: usize = 65;

/// Length of an Ethereum-style guardian pubkey (`keccak256(uncompressed_pk)[12..]`).
const GUARDIAN_PUBKEY_LEN: usize = 20;

/// Byte layout of `pinocchio::syscalls::sol_secp256k1_recover`'s `result`
/// buffer: a 64-byte uncompressed-without-prefix secp256k1 public key
/// (`X || Y`).
const SECP256K1_PUBKEY_RAW_LEN: usize = 64;

pub fn process(program_id: &Address, accounts: &mut [AccountView], data: &[u8]) -> ProgramResult {
    // Split the instruction data into fixed prefix + length-prefixed body.
    // Body is required on every submission so the program can re-verify the
    // digest against the bytes the caller is claiming the observation
    // covers; otherwise an attacker could land an arbitrary digest and
    // route balance updates to the wrong `(amount, token, recipient_chain)`.
    if data.len() < SUBMIT_FIXED_LEN + 2 {
        return Err(err(GlobalAccountantError::InvalidInstructionData));
    }
    let (fixed_bytes, rest) = data.split_at(SUBMIT_FIXED_LEN);
    let fixed_bytes: &[u8; SUBMIT_FIXED_LEN] = fixed_bytes
        .try_into()
        .map_err(|_| err(GlobalAccountantError::InvalidInstructionData))?;
    let body_len = u16::from_le_bytes([rest[0], rest[1]]) as usize;
    // Lower bound `BODY_MIN_LEN` (51-byte VAA header + 1-byte action) mirrors
    // `submit_vaas.rs`'s tighter check and surfaces malformed-body submissions
    // ~5K CU earlier — before the body→digest keccak roundtrip and before
    // `populate_routing_from_body`'s own 50-byte guard.
    const BODY_MIN_LEN: usize = 52;
    if !(BODY_MIN_LEN..=SUBMIT_BODY_MAX).contains(&body_len) || rest.len() < 2 + body_len {
        return Err(err(GlobalAccountantError::InvalidInstructionData));
    }
    let body_bytes = &rest[2..2 + body_len];

    let mut parsed = ParsedObservation::from_data(fixed_bytes)?;

    // Verify the body the caller claims this observation is about. The
    // double-keccak convention matches the Wormhole VAA digest (the same
    // function guardians sign and the Verify VAA Shim recomputes). Doing
    // this *before* any state mutation means a body/digest mismatch costs
    // one keccak roundtrip (~5k CU) and changes no PDAs — the cheapest
    // place to reject forged or truncated bodies.
    let computed = double_keccak256(body_bytes);
    if computed != parsed.digest {
        return Err(err(GlobalAccountantError::BodyDigestMismatch));
    }

    // Source the routing tuple from the body header now that the body is
    // proven authentic. Sourcing (chain, emitter, sequence) from caller-
    // controlled instruction data would let an attacker replay a signed body
    // under arbitrary namespaces — see the module-level wire-format doc and
    // the `submit_observations_routes_by_body_header_not_caller_supplied_prefix`
    // regression test in `programs/global-accountant/tests/submit_observations.rs`.
    parsed.populate_routing_from_body(body_bytes)?;

    // Accounts:
    //   0. `[WRITE, SIGNER]` submitter (fee payer; rent payer for fresh PDAs;
    //                       also payer for any lazy noreplay bitmap create AND
    //                       for any lazy Account PDA create on the quorum branch).
    //   1. `[WRITE]`         pending PDA.
    //   2. `[]`              GuardianSet PDA (Core Bridge).
    //   3. `[WRITE]`         NoReplay bitmap PDA. Read-only at pre-check
    //                       time, writable at commit time — the runtime
    //                       requires writability to be declared up-front, so
    //                       this slot is always WRITE.
    //   4. `[WRITE]`         DigestAccount PDA (opens on quorum).
    //   5. `[]`              system program (for `CreateAccount` / `Allocate`
    //                       / `Assign` across the pending PDA init AND the
    //                       noreplay bitmap lazy-init AND the Account PDA
    //                       lazy-inits).
    //   6. `[]`              NoReplay program (CPI target on quorum reach).
    //   7. `[]`              NoReplay authority PDA owned by this program;
    //                       signed via `invoke_signed` with seeds
    //                       `[NOREPLAY_AUTHORITY_SEED_PREFIX, authority_bump]`.
    //   8. `[WRITE]`         source-chain Account PDA at
    //                       `(b"account", source_chain, token_chain, token_address)`.
    //                       Required on every submission (the Solana runtime
    //                       requires writability up front), but only read /
    //                       written on the quorum-completing branch with a
    //                       Transfer payload. For non-Transfer payloads
    //                       (Attest / Other / non-quorum-completing
    //                       observations) the caller still supplies the slot
    //                       and the program never touches it.
    //   9. `[WRITE]`         destination-chain Account PDA at
    //                       `(b"account", recipient_chain, token_chain, token_address)`.
    //                       Same semantics as slot 8.
    //  10. `[WRITE]`         rent recipient for the pending PDA close on the
    //                       quorum-completing branch. Must equal the bucket's
    //                       recorded payer (the wallet that originally opened
    //                       the pending PDA); the program verifies the address
    //                       against `layout.payer` and rejects with
    //                       `PayerMismatch` otherwise. Decoupling submitter
    //                       from rent_recipient is what lets *any* guardian
    //                       (or relayer) close out quorum on behalf of the
    //                       original opener — the network does not know in
    //                       advance which submission will be the 13th, but
    //                       rent must always refund to whoever paid it. The
    //                       slot is required on every submission for runtime
    //                       account-meta declaration, but only credited on the
    //                       quorum-completing branch. Callers on non-quorum
    //                       submissions can pass any pubkey (the slot is not
    //                       read).
    //  11. `[]`              chain registration PDA at
    //                       `(b"chain_registration", body_chain.to_be_bytes())`.
    //                       Populated by the `register_chain` governance
    //                       instruction. Read on every submission to verify
    //                       the body header's `(emitter_chain, emitter_address)`
    //                       pair corresponds to a Token-Bridge-governance-
    //                       registered emitter. Mirrors CosmWasm's
    //                       `CHAIN_REGISTRATIONS` lookup at
    //                       `contract.rs:158-166`. A system-owned account at
    //                       this slot signals "no registration" and the
    //                       program returns `MissingChainRegistration`.
    let [submitter, pending_pda, guardian_set, noreplay_bucket, digest_pda, system_program_acc, noreplay_program, noreplay_authority, source_account_pda, dest_account_pda, rent_recipient, chain_registration_pda] =
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

    // Chain-registration cross-check mirrors CosmWasm `handle_observation`
    // (`contract.rs:158-166`). Without it an attacker with valid sigs for a
    // non-Token-Bridge VAA could route accounting against a fake emitter on
    // a real chain. PDA address verified against canonical seeds first.
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

    let pending_action = decide_pending_action(pending_pda, &parsed)?;

    match pending_action {
        PendingAction::Create => {
            create_pending_pda(program_id, submitter, pending_pda, &parsed)?;
        }
        PendingAction::WipeAndRecreate => {
            wipe_pending_pda(pending_pda, submitter)?;
            create_pending_pda(program_id, submitter, pending_pda, &parsed)?;
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

    let popcount = layout.signatures.count_ones();
    if popcount < PendingObservationsLayout::QUORUM_THRESHOLD {
        return Ok(());
    }

    // Quorum reached. Commit atomically: NoReplay flip, DigestAccount open,
    // balance accounting, pending close. Solana txs unwind everything if
    // any of these errors out, including the NoReplay bit.
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

    open_digest_inner(
        program_id,
        submitter,
        digest_pda,
        parsed.chain.to_be_bytes(),
        parsed.emitter,
        parsed.sequence.to_be_bytes(),
        parsed.digest,
        parsed.guardian_set_index,
        parsed.digest_pda_bump,
    )?;

    // Port of CosmWasm `commit_transfer`
    // (`cosmwasm/packages/accountant/src/contract.rs:109-126`). Transfer
    // payloads mutate two Account PDAs; Attest / Other skip balance work
    // but the rest of the commit still runs.
    match parse_token_bridge_payload(body_bytes).map_err(err)? {
        TokenBridgeAction::Transfer {
            amount,
            token_chain,
            token_address,
            recipient_chain,
        } => {
            // Source chain is the VAA emitter chain (== `parsed.chain`,
            // which the caller has already authenticated against the
            // signature). CosmWasm reads `t.key.emitter_chain()` for the
            // same purpose.
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
        TokenBridgeAction::Attest | TokenBridgeAction::Other => {
            // No balance work. Slots 8 and 9 are required for the runtime
            // account-meta declaration but the caller is expected to pass
            // sentinel addresses (e.g., the noreplay-authority PDA) — the
            // program intentionally does not touch them, so any account
            // shape is fine here.
        }
    }

    // Refund the recorded payer (separate from submitter so any guardian
    // can complete quorum on behalf of the bucket opener).
    let recorded_payer = layout.payer;
    close_pending_pda(pending_pda, rent_recipient, &recorded_payer)?;
    Ok(())
}

#[derive(Clone, Copy)]
struct ParsedObservation {
    /// Source: body's `keccak256(keccak256(body_bytes))` (signed by the guardian).
    /// Verified against the body bytes before any state work — the `digest` field
    /// in instruction data is what the guardian signature was generated against,
    /// and the body cross-check ensures it matches the supplied body.
    digest: [u8; 32],
    /// Source: body's header at byte offsets `[8..10]`, populated by `from_body`
    /// after the digest cross-check. Caller cannot lie about this.
    chain: u16,
    /// Source: body's header at byte offsets `[10..42]`. Caller cannot lie.
    emitter: [u8; 32],
    /// Source: body's header at byte offsets `[42..50]`. Caller cannot lie.
    sequence: u64,
    guardian_set_index: u32,
    guardian_index: u8,
    signature: [u8; SECP256K1_SIGNATURE_LEN],
    pending_pda_bump: u8,
    digest_pda_bump: u8,
}

impl ParsedObservation {
    /// Parse the non-routing fields from the fixed-size prefix. The routing
    /// tuple (chain, emitter, sequence) is left zeroed here and populated
    /// from `body[8..50]` once the body→digest cross-check has confirmed the
    /// body bytes match what the guardian signed.
    fn from_data(data: &[u8; SUBMIT_FIXED_LEN]) -> Result<Self, ProgramError> {
        let (digest_bytes, rest) = data.split_at(32);
        let (gsi_bytes, rest) = rest.split_at(4);
        let guardian_index = rest[0];
        let signature_bytes = &rest[1..1 + SECP256K1_SIGNATURE_LEN];
        let pending_pda_bump = rest[1 + SECP256K1_SIGNATURE_LEN];
        let digest_pda_bump = rest[1 + SECP256K1_SIGNATURE_LEN + 1];

        let digest_arr: [u8; 32] = digest_bytes
            .try_into()
            .map_err(|_| err(GlobalAccountantError::InvalidInstructionData))?;
        let gsi: [u8; 4] = gsi_bytes
            .try_into()
            .map_err(|_| err(GlobalAccountantError::InvalidInstructionData))?;
        let signature: [u8; SECP256K1_SIGNATURE_LEN] = signature_bytes
            .try_into()
            .map_err(|_| err(GlobalAccountantError::InvalidInstructionData))?;

        Ok(Self {
            digest: digest_arr,
            chain: 0,
            emitter: [0u8; 32],
            sequence: 0,
            guardian_set_index: u32::from_le_bytes(gsi),
            guardian_index,
            signature,
            pending_pda_bump,
            digest_pda_bump,
        })
    }

    /// Populate the routing tuple from the body header. The Wormhole body
    /// layout has `emitter_chain` at `[8..10]` (BE u16), `emitter_address` at
    /// `[10..42]`, and `sequence` at `[42..50]` (BE u64). Caller must have
    /// already proven `body` matches `self.digest` before calling this.
    fn populate_routing_from_body(&mut self, body: &[u8]) -> Result<(), ProgramError> {
        if body.len() < 50 {
            return Err(err(GlobalAccountantError::InvalidInstructionData));
        }
        self.chain = u16::from_be_bytes([body[8], body[9]]);
        self.emitter = body[10..42]
            .try_into()
            .map_err(|_| err(GlobalAccountantError::InvalidInstructionData))?;
        self.sequence = u64::from_be_bytes(
            body[42..50]
                .try_into()
                .map_err(|_| err(GlobalAccountantError::InvalidInstructionData))?,
        );
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PendingAction {
    /// PDA does not exist yet — allocate, assign, and write a fresh layout.
    Create,
    /// PDA exists but for an older guardian set — refund the recorded payer,
    /// wipe, and re-create under the new index.
    WipeAndRecreate,
    /// PDA exists for the same guardian set and same digest — just toggle the
    /// bitmap bit.
    Continue,
}

/// Decide what to do with the pending PDA for this observation.
///
/// "System-owned with zero data" means "fresh slot — create". "Non-system
/// owner with data" means "ours, already accumulating — compare". The runtime
/// guarantees no other program can write `PendingObservationsLayout::LEN`
/// bytes at the canonical address; we accept that invariant rather than
/// importing the program ID for equality (Pinocchio determines program ID at
/// deploy-time, not as a `const`).
///
/// Per-digest PDA seeds mean the digest-mismatch case never lands in this
/// function: a different digest produces a different canonical address, and
/// that address is either uninitialised (`Create`) or already filled by some
/// prior observation under the *same* digest (`Continue` / rotation).
/// `DigestForgery` is therefore retired — every PDA loaded here was opened
/// under exactly the digest we are accumulating against.
fn decide_pending_action(
    pending_pda: &AccountView,
    parsed: &ParsedObservation,
) -> Result<PendingAction, ProgramError> {
    let owner_is_system = pending_pda.owner() == &pinocchio_system::ID;
    let data_len = pending_pda.data_len();

    if owner_is_system && data_len == 0 {
        return Ok(PendingAction::Create);
    }
    if owner_is_system {
        // System-owned with non-zero data is unreachable on Solana: the system
        // program cannot Allocate space at a PDA address without us first
        // signing an Assign via invoke_signed. Reject loudly rather than
        // routing to init_or_upgrade_pda, which would error on the data_len
        // guard anyway — surfacing the impossibility here makes the intent
        // explicit instead of relying on a downstream defence.
        return Err(err(GlobalAccountantError::InvalidPda));
    }

    // Non-system owner: must be us. Load and compare.
    let existing = pending::load(pending_pda)?;
    if existing.guardian_set_index < parsed.guardian_set_index {
        return Ok(PendingAction::WipeAndRecreate);
    }
    if existing.guardian_set_index > parsed.guardian_set_index {
        return Err(err(GlobalAccountantError::StaleGuardianSet));
    }
    // Digest equality is guaranteed by construction: the PDA's seeds include
    // the digest, and `create_pending_pda` rejects any non-canonical bump.
    // Belt-and-braces: if somehow a layout's recorded digest disagrees with
    // the observation's (e.g., a buggy upgrade path), refuse the submission.
    // This branch is unreachable in normal operation.
    if existing.digest != parsed.digest {
        return Err(err(GlobalAccountantError::DigestForgery));
    }
    Ok(PendingAction::Continue)
}

/// Allocate the pending PDA under
/// `(b"pending", chain, emitter, sequence, digest)` and stamp the freshly-zeroed
/// layout. Including the digest in the seed tuple is what lets fork/reorg
/// observations (same chain/emitter/sequence, different digest) accumulate in
/// parallel sibling buckets rather than getting stuck on a `DigestForgery`
/// rejection.
fn create_pending_pda(
    program_id: &Address,
    submitter: &AccountView,
    pending_pda: &mut AccountView,
    parsed: &ParsedObservation,
) -> ProgramResult {
    // Canonical-bump enforcement, mirroring `open_digest_inner`. A
    // non-canonical bump that still produces a valid off-curve PDA would let
    // an attacker mint sibling pending PDAs for the same logical key.
    let chain_be = parsed.chain.to_be_bytes();
    let sequence_be = parsed.sequence.to_be_bytes();
    let (_expected, canonical_bump) = Address::find_program_address(
        &[
            PENDING_SEED_PREFIX,
            &chain_be,
            &parsed.emitter,
            &sequence_be,
            &parsed.digest,
        ],
        program_id,
    );
    if parsed.pending_pda_bump != canonical_bump {
        return Err(err(GlobalAccountantError::InvalidPda));
    }

    let bump_seed = [parsed.pending_pda_bump];
    let seeds = [
        Seed::from(PENDING_SEED_PREFIX),
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

    let slot = Clock::get()?.slot;
    let mut layout: PendingObservationsLayout = bytemuck::Zeroable::zeroed();
    layout.digest = parsed.digest;
    layout.payer = *submitter.address().as_array();
    layout.guardian_set_index = parsed.guardian_set_index;
    layout.signatures = 0;
    layout.created_at_slot = slot;
    layout.chain = parsed.chain;
    pending::store(pending_pda, &layout)
}

/// Refund the recorded payer and zero the account. Used by the quorum-commit
/// branch. The caller has already loaded the layout to read `payer` and
/// `guardian_set_index`; re-loading here would borrow twice, so we pass the
/// recorded payer in.
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

/// Rotation-wipe variant: the `submitter` is *not* the recorded payer
/// (rotation means a new submitter is opening a fresh bucket under the new
/// set), so we cannot use `close_pending_pda` (it would error with
/// `PayerMismatch`).
///
/// We refund by directly debiting the PDA's lamports and crediting the
/// submitter's account that the runtime supplied. The rotation case does not
/// pass the original payer as an explicit account (the wire shape carries
/// only the new submitter), so the original payer's rent is forfeit to the
/// new submitter as a small reward for paying the rotation cost.
///
/// This is a deliberate simplification: passing the original payer account
/// every time would balloon the account list for the uncommon-but-not-rare
/// rotation case. The forfeit is bounded (~$0.10) and the alternative —
/// gas-sponsored rent recovery via the explicit `close_pending` ix — remains
/// available for any payer who notices ahead of rotation.
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

/// Inline signature verification via `secp256k1_recover`. The recovered
/// pubkey is keccak-hashed and compared to the 20-byte Ethereum-style guardian
/// key stored in the Core Bridge GuardianSet PDA.
fn verify_signature(
    guardian_set: &AccountView,
    expected_guardian_set_index: u32,
    guardian_index: u8,
    digest: &[u8; 32],
    signature: &[u8; SECP256K1_SIGNATURE_LEN],
) -> ProgramResult {
    let data = guardian_set.try_borrow()?;
    let expected_key = read_guardian_key(&data, expected_guardian_set_index, guardian_index)?;
    drop(data);

    // `signature[64]` is the 1-byte recovery id; the syscall takes it as a
    // `u64`. recovery id ∈ {0, 1, 2, 3} — values >= 4 indicate a malformed
    // signature.
    let recovery_id = signature[64];
    if recovery_id >= 4 {
        return Err(err(GlobalAccountantError::InvalidSignature));
    }

    let mut recovered = [0u8; SECP256K1_PUBKEY_RAW_LEN];
    let rc = secp256k1_recover(digest, recovery_id as u64, &signature[..64], &mut recovered);
    if rc != 0 {
        return Err(err(GlobalAccountantError::InvalidSignature));
    }

    // Ethereum-style guardian pubkey: `keccak256(uncompressed_pk)[12..]`.
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
/// On-disk layout (see
/// `svm/wormhole-core-shims/crates/definitions/src/zero_copy/guardian_set.rs`):
///
/// | offset | size | field              |
/// |--------|------|--------------------|
/// | 0      | 4    | guardian_set_index |
/// | 4      | 4    | keys_len           |
/// | 8      | 20*N | keys (Ethereum-style 20-byte pubkeys) |
/// | 8+20N  | 4    | creation_time      |
/// | 12+20N | 4    | expiration_time    |
fn read_guardian_key(
    data: &[u8],
    expected_index: u32,
    guardian_index: u8,
) -> Result<[u8; GUARDIAN_PUBKEY_LEN], ProgramError> {
    if data.len() < 8 {
        return Err(err(GlobalAccountantError::InvalidPda));
    }
    let on_chain_index = u32::from_le_bytes(
        data[..4]
            .try_into()
            .map_err(|_| err(GlobalAccountantError::InvalidPda))?,
    );
    if on_chain_index != expected_index {
        return Err(err(GlobalAccountantError::InvalidGuardianIndex));
    }
    let keys_len = u32::from_le_bytes(
        data[4..8]
            .try_into()
            .map_err(|_| err(GlobalAccountantError::InvalidPda))?,
    );
    if (guardian_index as u32) >= keys_len {
        return Err(err(GlobalAccountantError::InvalidGuardianIndex));
    }
    let start = 8 + (guardian_index as usize) * GUARDIAN_PUBKEY_LEN;
    let end = start + GUARDIAN_PUBKEY_LEN;
    if data.len() < end {
        return Err(err(GlobalAccountantError::InvalidPda));
    }
    let mut key = [0u8; GUARDIAN_PUBKEY_LEN];
    key.copy_from_slice(&data[start..end]);
    Ok(key)
}

// `sol_secp256k1_recover` / `sol_keccak256` are re-exported by Pinocchio for
// the SBF target only. The host-cfg variants below let the program crate
// build on `cargo check` outside of `cargo build-sbf`; they are not reached
// from any mollusk test (mollusk loads the SBF `.so`, which uses the syscall
// path).
#[cfg(any(target_os = "solana", target_arch = "bpf"))]
fn secp256k1_recover(
    hash: &[u8; 32],
    recovery_id: u64,
    signature: &[u8],
    result: &mut [u8],
) -> u64 {
    // SAFETY: pinocchio re-exports the Solana syscall ABI. The buffers match
    // the syscall's documented layout: 32-byte hash, 64-byte signature
    // (`r||s`), 64-byte result.
    unsafe {
        pinocchio::syscalls::sol_secp256k1_recover(
            hash.as_ptr(),
            recovery_id,
            signature.as_ptr(),
            result.as_mut_ptr(),
        )
    }
}

#[cfg(not(any(target_os = "solana", target_arch = "bpf")))]
fn secp256k1_recover(
    _hash: &[u8; 32],
    _recovery_id: u64,
    _signature: &[u8],
    _result: &mut [u8],
) -> u64 {
    1
}

use crate::hash::{double_keccak256, keccak256};
