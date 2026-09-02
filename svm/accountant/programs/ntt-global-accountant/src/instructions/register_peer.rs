//! `register_peer` — NTT transceiver peer registration (`PEER_INFO_PREFIX`).
//!
//! Mirrors the CosmWasm `ntt-global-accountant` peer branch
//! (`src/contract.rs:577-633`). A transfer-class NTT VAA whose inner payload
//! leads with `WormholeTransceiver::PEER_INFO_PREFIX` registers the emitting
//! transceiver `(emitter_chain, emitter_address)`'s peer on `dest_chain` to
//! `peer_address`, but only after the hub-match validation below.
//!
//! Hub-match logic, byte-for-byte with the CosmWasm source:
//!   - Load the PEER's hub: `peer_hub = TRANSCEIVER_TO_HUB[(dest_chain,
//!     peer_address)]` (contract.rs:584-589). Missing ⇒ `MissingHubRegistration`
//!     (here `MissingTransceiverHub`).
//!   - Reject duplicate peer: `TRANSCEIVER_PEER[(emitter_chain, emitter_address,
//!     dest_chain)]` must not already exist (contract.rs:592-596).
//!   - Resolve this transceiver's own hub `this_hub = TRANSCEIVER_TO_HUB[
//!     (emitter_chain, emitter_address)]` (contract.rs:598-617):
//!       * if it exists, require `this_hub == peer_hub`, else
//!         `PeerRegistrationMismatch` ("peer hub does not match", line 602-603);
//!       * else if the peer IS its own hub (`peer_hub == (dest_chain,
//!         peer_address)`), adopt it as this transceiver's hub — WRITE a
//!         `TransceiverHubLayout` at `(emitter_chain, emitter_address)` pointing
//!         at `peer_hub` (line 605-612);
//!       * else `PeerBeforeHub` ("ignoring attempt to register peer before hub",
//!         line 613-615).
//!   - Write the peer PDA = `peer_address` (line 619-621).
//!
//! As with `register_hub`, the VAA emitter is the transceiver itself (no
//! governance-emitter pin); authenticity is the guardian quorum.

use anchor_lang::prelude::*;
use anchor_lang::solana_program::program_error::ProgramError;

use accountant_operational_core::hash::double_keccak256;
use accountant_operational_core::cpi::{noreplay, shim};
use accountant_operational_core::support::pda_init::init_or_upgrade_pda;
use accountant_operational_core::ProgramResult;

use crate::definitions::{
    parse_transceiver_registration, parse_vaa_namespace_key, GlobalAccountantError,
    TransceiverHubLayout, TransceiverPeerLayout, TRANSCEIVER_HUB_SEED_PREFIX,
    TRANSCEIVER_PEER_SEED_PREFIX, VaaBodyHeader,
};
use crate::err;

/// Wire format for the `register_peer` instruction data (after the 1-byte
/// dispatch discriminator):
///
/// | offset | size     | field             |
/// |--------|----------|-------------------|
/// | 0      | 1        | guardian_set_bump |
/// | 1      | 1        | this_hub_bump     |  (for the adopt-case init)
/// | 2      | 1        | peer_bump         |
/// | 3      | 2        | body_len (LE)     |
/// | 5      | body_len | body              |
const REGISTER_PEER_FIXED_LEN: usize = 1 + 1 + 1 + 2;

/// Maximum VAA body size accepted, leaving headroom inside Solana's 1232-byte tx
/// envelope. A peer VAA is the 51-byte header + a small registration payload.
const REGISTER_PEER_BODY_MAX: usize = 512;

pub fn process(program_id: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    // ----- (1) Parse wire data -----
    if data.len() < REGISTER_PEER_FIXED_LEN {
        return Err(err(GlobalAccountantError::InvalidInstructionData));
    }
    let guardian_set_bump = data[0];
    let this_hub_bump = data[1];
    let peer_bump = data[2];
    let body_len = u16::from_le_bytes([data[3], data[4]]) as usize;
    if !(VaaBodyHeader::LEN..=REGISTER_PEER_BODY_MAX).contains(&body_len)
        || data.len() != REGISTER_PEER_FIXED_LEN + body_len
    {
        return Err(err(GlobalAccountantError::InvalidInstructionData));
    }
    let body_bytes = &data[REGISTER_PEER_FIXED_LEN..REGISTER_PEER_FIXED_LEN + body_len];

    // ----- (2) Compute digest -----
    let digest = double_keccak256(body_bytes);

    // Accounts:
    //   0. `[WRITE, SIGNER]` payer — rent for the fresh peer PDA (and, in the
    //                        adopt case, the fresh own-hub PDA).
    //   1. `[]`              Verify VAA Shim program (CPI target).
    //   2. `[]`              Core Bridge `GuardianSet` PDA.
    //   3. `[]`              `GuardianSignatures` PDA.
    //   4. `[]`              Peer's hub PDA at `(b"transceiver_hub", dest_chain_be,
    //                        peer_address)` — READ-ONLY input; must already exist.
    //   5. `[WRITE]`         This transceiver's own hub PDA at
    //                        `(b"transceiver_hub", emitter_chain_be, emitter_address)`
    //                        — read; WRITTEN (init) only in the adopt case where
    //                        the peer is its own hub.
    //   6. `[WRITE]`         Peer PDA at `(b"transceiver_peer", emitter_chain_be,
    //                        emitter_address, dest_chain_be)` — WRITTEN (init).
    //   7. `[WRITE]`         NoReplay bitmap PDA. Pre-check then mark-used CPI.
    //   8. `[]`              NoReplay program (CPI target).
    //   9. `[]`              NoReplay authority PDA owned by this program.
    //  10. `[]`              system program.
    let [payer, _verify_vaa_shim_program, guardian_set, guardian_signatures, peer_hub_pda, own_hub_pda, peer_pda, noreplay_bucket, noreplay_program, noreplay_authority, system_program_acc] =
        accounts
    else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    if !payer.is_signer {
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
    let header = parse_vaa_namespace_key(body_bytes).map_err(err)?;
    let emitter_chain = header.chain;
    let emitter_address = header.emitter;
    let sequence = header.sequence;

    // ----- (5) NoReplay pre-check -----
    if noreplay::is_marked(
        noreplay_bucket,
        noreplay_authority.key,
        emitter_chain,
        &emitter_address,
        sequence,
    )? {
        return Err(err(GlobalAccountantError::AlreadyAccounted));
    }

    // ----- (6) Parse the registration payload -----
    let payload = &body_bytes[VaaBodyHeader::LEN..];
    let reg = parse_transceiver_registration(payload).map_err(err)?;
    let dest_chain = reg.dest_chain;
    let peer_address = reg.peer_address;

    // ----- (7) Load the PEER's hub (must exist) -----
    //
    // contract.rs:584-589 — `TRANSCEIVER_TO_HUB.load((dest_chain, peer_address))`,
    // mapping a load error to `MissingHubRegistration`.
    let peer_hub = read_hub_required(program_id, peer_hub_pda, dest_chain, &peer_address)?;

    // ----- (8) Reject duplicate peer -----
    //
    // contract.rs:592-596 — `TRANSCEIVER_PEER[(emitter_chain, emitter_address,
    // dest_chain)]` must not already exist.
    let dest_chain_be = dest_chain.to_be_bytes();
    let emitter_chain_be = emitter_chain.to_be_bytes();
    let (expected_peer_pda, canonical_peer_bump) = Pubkey::find_program_address(
        &[
            TRANSCEIVER_PEER_SEED_PREFIX,
            &emitter_chain_be,
            &emitter_address,
            &dest_chain_be,
        ],
        program_id,
    );
    if peer_pda.key != &expected_peer_pda || peer_bump != canonical_peer_bump {
        return Err(err(GlobalAccountantError::InvalidPda));
    }
    if peer_pda.owner != &anchor_lang::solana_program::system_program::ID {
        return Err(err(GlobalAccountantError::DuplicateTransceiverPeer));
    }

    // ----- (9) Resolve / adopt this transceiver's own hub -----
    //
    // contract.rs:598-617. The own-hub PDA address is always validated; whether
    // it is read or written depends on the branch.
    let (expected_own_hub, canonical_own_hub_bump) = Pubkey::find_program_address(
        &[
            TRANSCEIVER_HUB_SEED_PREFIX,
            &emitter_chain_be,
            &emitter_address,
        ],
        program_id,
    );
    if own_hub_pda.key != &expected_own_hub {
        return Err(err(GlobalAccountantError::InvalidPda));
    }

    if own_hub_pda.owner != &anchor_lang::solana_program::system_program::ID {
        // This transceiver already has a hub — it must match the peer's hub.
        let this_hub = read_hub(own_hub_pda)?;
        if this_hub.hub_chain != peer_hub.0 || this_hub.hub_address != peer_hub.1 {
            return Err(err(GlobalAccountantError::PeerRegistrationMismatch));
        }
    } else {
        // No known hub. Adopt only if the peer is itself a hub (its hub points at
        // itself): peer_hub == (dest_chain, peer_address).
        if peer_hub.0 == dest_chain && peer_hub.1 == peer_address {
            if this_hub_bump != canonical_own_hub_bump {
                return Err(err(GlobalAccountantError::InvalidPda));
            }
            let bump_seed = [this_hub_bump];
            let seeds: &[&[u8]] = &[
                TRANSCEIVER_HUB_SEED_PREFIX,
                &emitter_chain_be,
                &emitter_address,
                &bump_seed,
            ];
            init_or_upgrade_pda(
                payer,
                own_hub_pda,
                program_id,
                seeds,
                TransceiverHubLayout::LEN as u64,
            )?;
            let mut layout: TransceiverHubLayout = bytemuck::Zeroable::zeroed();
            layout.tag = TransceiverHubLayout::TAG;
            layout.chain = emitter_chain;
            layout.hub_chain = peer_hub.0;
            layout.address = emitter_address;
            layout.hub_address = peer_hub.1;
            {
                let mut data_mut = own_hub_pda.try_borrow_mut_data()?;
                if data_mut.len() != TransceiverHubLayout::LEN {
                    return Err(err(GlobalAccountantError::InvalidPda));
                }
                data_mut.copy_from_slice(bytemuck::bytes_of(&layout));
            }
        } else {
            return Err(err(GlobalAccountantError::PeerBeforeHub));
        }
    }

    // ----- (10) Write the peer PDA -----
    let bump_seed = [peer_bump];
    let seeds: &[&[u8]] = &[
        TRANSCEIVER_PEER_SEED_PREFIX,
        &emitter_chain_be,
        &emitter_address,
        &dest_chain_be,
        &bump_seed,
    ];
    init_or_upgrade_pda(
        payer,
        peer_pda,
        program_id,
        seeds,
        TransceiverPeerLayout::LEN as u64,
    )?;
    let mut layout: TransceiverPeerLayout = bytemuck::Zeroable::zeroed();
    layout.tag = TransceiverPeerLayout::TAG;
    layout.chain = emitter_chain;
    layout.dest_chain = dest_chain;
    layout.address = emitter_address;
    layout.peer_address = peer_address;
    {
        let mut data_mut = peer_pda.try_borrow_mut_data()?;
        if data_mut.len() != TransceiverPeerLayout::LEN {
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

/// Read a `TransceiverHub` PDA at `(b"transceiver_hub", chain_be, address)` that
/// MUST exist, returning its `(hub_chain, hub_address)`. Canonical-address
/// checked; a missing (system-owned) PDA is `MissingTransceiverHub`.
fn read_hub_required(
    program_id: &Pubkey,
    hub_pda: &AccountInfo,
    chain: u16,
    address: &[u8; 32],
) -> core::result::Result<(u16, [u8; 32]), ProgramError> {
    let chain_be = chain.to_be_bytes();
    let (expected, _) = Pubkey::find_program_address(
        &[TRANSCEIVER_HUB_SEED_PREFIX, &chain_be, address],
        program_id,
    );
    if hub_pda.key != &expected {
        return Err(err(GlobalAccountantError::InvalidPda));
    }
    if hub_pda.owner == &anchor_lang::solana_program::system_program::ID {
        return Err(err(GlobalAccountantError::MissingTransceiverHub));
    }
    let layout = read_hub(hub_pda)?;
    Ok((layout.hub_chain, layout.hub_address))
}

/// Deserialize a program-owned `TransceiverHub` PDA. Validates length and tag.
fn read_hub(hub_pda: &AccountInfo) -> core::result::Result<TransceiverHubLayout, ProgramError> {
    let data = hub_pda.try_borrow_data()?;
    if data.len() != TransceiverHubLayout::LEN {
        return Err(err(GlobalAccountantError::InvalidPda));
    }
    let layout = bytemuck::from_bytes::<TransceiverHubLayout>(&data);
    if layout.tag != TransceiverHubLayout::TAG {
        return Err(err(GlobalAccountantError::InvalidPda));
    }
    Ok(*layout)
}
