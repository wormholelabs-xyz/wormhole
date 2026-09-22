//! NTT transfer routing, as the CosmWasm `handle_ntt_vaa` transfer branch: the sender must be
//! cross-registered with its peer on the recipient chain, and balances are keyed on the
//! sender's hub as the token identity.

use anchor_lang::prelude::*;

use accountant_operational_core::support::pda;
use accountant_operational_core::{transfer, ProgramResult};

use crate::definitions::{
    GlobalAccountantError, TransceiverHubKey, TransceiverPeerKey, TransceiverPeerLayout, Uint256,
};
use crate::err;

/// SECURITY: the peer's entry for the sender's chain must name the sender. A transceiver
/// writes only its own peer entries. So one transceiver cannot move the hub's balances to a
/// counterparty that did not register it.
pub fn check_route(
    program_id: &Pubkey,
    peer_src_pda: &AccountInfo,
    peer_dst_pda: &AccountInfo,
    chain: u16,
    sender: [u8; 32],
    recipient_chain: u16,
) -> ProgramResult {
    // Sender's peer entry: `(chain, sender, recipient_chain)`, derived from the VAA fields.
    // Its value is the counterparty on the recipient chain.
    pda::check(
        program_id,
        peer_src_pda,
        &TransceiverPeerKey::new(chain, sender, recipient_chain),
    )?;
    let source_peer = pda::read_if_initialised::<TransceiverPeerLayout>(program_id, peer_src_pda)?
        .ok_or_else(|| err(GlobalAccountantError::MissingSourcePeer))?
        .peer_address;

    // Counterparty's peer entry: `(recipient_chain, source_peer, chain)`, derived from the
    // value just read. Its value must be the sender.
    pda::check(
        program_id,
        peer_dst_pda,
        &TransceiverPeerKey::new(recipient_chain, source_peer, chain),
    )?;
    let destination_peer =
        pda::read_if_initialised::<TransceiverPeerLayout>(program_id, peer_dst_pda)?
            .ok_or_else(|| err(GlobalAccountantError::MissingDestinationPeer))?
            .peer_address;
    if destination_peer != sender {
        return Err(err(GlobalAccountantError::PeersNotCrossRegistered));
    }
    Ok(())
}

/// [`check_route`], then move `amount` between the hub token's balances.
#[allow(clippy::too_many_arguments)]
pub fn apply_routed<'info>(
    program_id: &Pubkey,
    payer: &AccountInfo<'info>,
    hub: TransceiverHubKey,
    peer_src_pda: &AccountInfo<'info>,
    peer_dst_pda: &AccountInfo<'info>,
    source_balance: &AccountInfo<'info>,
    dest_balance: &AccountInfo<'info>,
    chain: u16,
    sender: [u8; 32],
    recipient_chain: u16,
    amount: Uint256,
) -> ProgramResult {
    check_route(
        program_id,
        peer_src_pda,
        peer_dst_pda,
        chain,
        sender,
        recipient_chain,
    )?;

    transfer::apply_transfer(
        program_id,
        payer,
        source_balance,
        dest_balance,
        chain,
        recipient_chain,
        hub.chain(),
        &hub.address,
        amount,
    )
}
