//! `register_hub` — NTT transceiver hub registration (`INFO_PREFIX`).
//!
//! Mirrors the CosmWasm `ntt-global-accountant` hub branch
//! (`src/contract.rs:552-576`): a transfer-class NTT VAA whose inner payload
//! leads with `WormholeTransceiver::INFO_PREFIX`. Only **Locking**-mode
//! transceiver-info messages register a hub; Burning-mode messages are rejected
//! (CosmWasm `bail!("ignoring non-locking NTT initialization")`). On Locking,
//! the hub mapping points the transceiver `(emitter_chain, emitter_address)` at
//! itself: `TRANSCEIVER_TO_HUB[(chain, sender)] = (chain, sender)`.
//!
//! Structurally identical to `register_relayer_chain`: verify the signed VAA via
//! the Shim CPI, NoReplay-mark for replay protection, then write a tagged layout
//! PDA. The divergences are the payload parser (`parse_transceiver_info`) and the
//! destination layout/seed (`TransceiverHubLayout` at `TRANSCEIVER_HUB_SEED_PREFIX`).
//!
//! Note: unlike the `register_relayer_chain` governance path, a hub VAA's emitter
//! is the transceiver itself — there is no governance-emitter pin. The
//! authenticity guarantee is the guardian quorum verified by the Shim; the
//! hub key is taken directly from the body header `(emitter_chain, emitter_address)`.

use pinocchio::{
    cpi::{Seed, Signer},
    error::ProgramError,
    AccountView, Address, ProgramResult,
};

use crate::definitions::{
    parse_transceiver_info, parse_vaa_namespace_key, GlobalAccountantError, TransceiverHubLayout,
    TRANSCEIVER_HUB_SEED_PREFIX, VAA_BODY_HEADER_LEN,
};
use crate::err;
use accountant_operational_core::hash::double_keccak256;
use accountant_operational_core::instructions::{noreplay, pda_init::init_or_upgrade_pda, shim};

/// Wire format for the `register_hub` instruction data (after the 1-byte
/// dispatch discriminator):
///
/// | offset | size     | field             |
/// |--------|----------|-------------------|
/// | 0      | 1        | guardian_set_bump |
/// | 1      | 1        | hub_bump          |
/// | 2      | 2        | body_len (LE)     |
/// | 4      | body_len | body              |
const REGISTER_HUB_FIXED_LEN: usize = 1 + 1 + 2;

/// Maximum VAA body size accepted, leaving headroom inside Solana's 1232-byte tx
/// envelope. A hub VAA is the 51-byte header + a small transceiver-info payload.
const REGISTER_HUB_BODY_MAX: usize = 512;

pub fn process(program_id: &Address, accounts: &mut [AccountView], data: &[u8]) -> ProgramResult {
    // ----- (1) Parse wire data -----
    if data.len() < REGISTER_HUB_FIXED_LEN {
        return Err(err(GlobalAccountantError::InvalidInstructionData));
    }
    let guardian_set_bump = data[0];
    let hub_bump = data[1];
    let body_len = u16::from_le_bytes([data[2], data[3]]) as usize;
    if !(VAA_BODY_HEADER_LEN..=REGISTER_HUB_BODY_MAX).contains(&body_len)
        || data.len() != REGISTER_HUB_FIXED_LEN + body_len
    {
        return Err(err(GlobalAccountantError::InvalidInstructionData));
    }
    let body_bytes = &data[REGISTER_HUB_FIXED_LEN..REGISTER_HUB_FIXED_LEN + body_len];

    // ----- (2) Compute digest -----
    let digest = double_keccak256(body_bytes);

    // Accounts:
    //   0. `[WRITE, SIGNER]` payer — rent for the fresh hub PDA.
    //   1. `[]`              Verify VAA Shim program (CPI target).
    //   2. `[]`              Core Bridge `GuardianSet` PDA.
    //   3. `[]`              `GuardianSignatures` PDA.
    //   4. `[WRITE]`         Transceiver-hub PDA at
    //                        `(b"transceiver_hub", emitter_chain_be, emitter_address)`.
    //                        Must be system-owned (uninitialised) on entry; a
    //                        duplicate hub rejects (CosmWasm "hub entry already exists").
    //   5. `[WRITE]`         NoReplay bitmap PDA. Pre-check then mark-used CPI.
    //   6. `[]`              NoReplay program (CPI target).
    //   7. `[]`              NoReplay authority PDA owned by this program.
    //   8. `[]`              system program.
    let [payer, _verify_vaa_shim_program, guardian_set, guardian_signatures, hub_pda, noreplay_bucket, noreplay_program, noreplay_authority, system_program_acc] =
        accounts
    else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    if !payer.is_signer() {
        return Err(ProgramError::MissingRequiredSignature);
    }

    // ----- (3) Shim CPI to verify the digest -----
    shim::verify_vaa(
        guardian_set,
        guardian_signatures,
        &digest,
        guardian_set_bump,
    )?;

    // ----- (4) Parse the body header -----
    //
    // The hub key is the transceiver's own emitter `(chain, address)`; the
    // sequence keys the NoReplay slot. There is no governance-emitter pin — a hub
    // message is emitted by the transceiver itself, authenticated by quorum.
    let header = parse_vaa_namespace_key(body_bytes).map_err(err)?;
    let emitter_chain = header.chain;
    let emitter_address = header.emitter;
    let sequence = header.sequence;

    // ----- (5) NoReplay pre-check -----
    if noreplay::is_marked(
        noreplay_bucket,
        noreplay_authority.address(),
        emitter_chain,
        &emitter_address,
        sequence,
    )? {
        return Err(err(GlobalAccountantError::AlreadyAccounted));
    }

    // ----- (6) Parse the transceiver-info payload -----
    //
    // CosmWasm only acts on Locking-mode info; Burning is rejected
    // (`bail!("ignoring non-locking NTT initialization")`). We reject too, leaving
    // the NoReplay slot unconsumed so a later upgrade could process the VAA.
    let payload = &body_bytes[VAA_BODY_HEADER_LEN..];
    let info = parse_transceiver_info(payload).map_err(err)?;
    if !info.locking {
        return Err(err(GlobalAccountantError::NotLockingHub));
    }

    // ----- (7) Canonical PDA enforcement -----
    let chain_be = emitter_chain.to_be_bytes();
    let (expected_pda, canonical_bump) = Address::find_program_address(
        &[TRANSCEIVER_HUB_SEED_PREFIX, &chain_be, &emitter_address],
        program_id,
    );
    if hub_pda.address() != &expected_pda || hub_bump != canonical_bump {
        return Err(err(GlobalAccountantError::InvalidPda));
    }

    // ----- (8) Reject duplicate hub -----
    //
    // CosmWasm bails if a hub entry already exists. An existing hub PDA is
    // program-owned; only a system-owned (uninitialised) PDA is allowed.
    if hub_pda.owner() != &pinocchio_system::ID {
        return Err(err(GlobalAccountantError::DuplicateTransceiverHub));
    }

    // ----- (9) Init the hub PDA -----
    let bump_seed = [hub_bump];
    let seeds = [
        Seed::from(TRANSCEIVER_HUB_SEED_PREFIX),
        Seed::from(chain_be.as_slice()),
        Seed::from(emitter_address.as_slice()),
        Seed::from(bump_seed.as_slice()),
    ];
    let signer = Signer::from(&seeds);
    init_or_upgrade_pda(
        payer,
        hub_pda,
        program_id,
        signer,
        TransceiverHubLayout::LEN as u64,
    )?;

    // ----- (10) Write the hub layout (hub points to itself) -----
    let mut layout: TransceiverHubLayout = bytemuck::Zeroable::zeroed();
    layout.tag = TransceiverHubLayout::TAG;
    layout.chain = emitter_chain;
    layout.hub_chain = emitter_chain;
    layout.address = emitter_address;
    layout.hub_address = emitter_address;
    {
        let mut data_mut = hub_pda.try_borrow_mut()?;
        if data_mut.len() != TransceiverHubLayout::LEN {
            return Err(err(GlobalAccountantError::InvalidPda));
        }
        data_mut.copy_from_slice(bytemuck::bytes_of(&layout));
    }

    // ----- (11) NoReplay mark-used -----
    noreplay::mark_used(
        payer,
        noreplay_bucket,
        noreplay_program,
        noreplay_authority,
        system_program_acc,
        program_id,
        emitter_chain,
        &emitter_address,
        sequence,
    )?;

    Ok(())
}
