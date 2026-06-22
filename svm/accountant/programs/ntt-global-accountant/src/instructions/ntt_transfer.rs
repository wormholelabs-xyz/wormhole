//! NTT transfer-flow applicator: the product-specific balance work invoked on
//! the quorum-completing branch of `submit_observations` and after the Shim
//! verification in `submit_vaas`.
//!
//! Mirrors the CosmWasm `handle_observation` / `handle_ntt_vaa` flow (frozen D0
//! spec). The digest/quorum/NoReplay/commit-log machinery is shared with WTT via
//! `accountant-operational-core`; only this transfer flow is NTT-specific:
//!
//!   1. Relayer-unwrap: if the registered relayer PDA for `emitter_chain`
//!      matches `emitter_address`, parse the `DeliveryInstruction` to recover
//!      `(sender, inner_payload)`; otherwise `sender = emitter_address` and the
//!      payload is the body payload as-is.
//!   2. Hub-substitute: read the `TransceiverHub` PDA at `(emitter_chain, sender)`
//!      to obtain the `(hub_chain, hub_address)` token identity.
//!   3. Parse the NTT `TransceiverMessage` to recover the normalized `amount`
//!      and `recipient_chain`.
//!   4. Peer cross-check: the `TransceiverPeer` PDA at `(emitter_chain, sender,
//!      recipient_chain)` gives the destination peer; the reverse PDA at
//!      `(recipient_chain, source_peer, emitter_chain)` must register back to
//!      `sender`.
//!   5. Apply accounting against the HUB token identity (never the NTT
//!      `source_token`): source balance `(emitter_chain, hub_chain, hub_address)`
//!      `lock_or_burn(amount)`; dest balance `(recipient_chain, hub_chain,
//!      hub_address)` `unlock_or_mint(amount)`.

use pinocchio::{account::Ref, error::ProgramError, AccountView, Address, ProgramResult};

use accountant_operational_core::state::account as account_state;

use crate::definitions::{
    parse_delivery_instruction, parse_ntt_transfer, GlobalAccountantError,
    RelayerChainRegistrationLayout, TransceiverHubLayout, TransceiverPeerLayout, Uint256,
    ACCOUNT_SEED_PREFIX, RELAYER_CHAIN_REGISTRATION_SEED_PREFIX, TRANSCEIVER_HUB_SEED_PREFIX,
    TRANSCEIVER_PEER_SEED_PREFIX, VAA_BODY_HEADER_LEN,
};
use crate::err;

/// The five NTT transfer accounts that trail the quorum accounts in both the
/// `submit_observations` and `submit_vaas` layouts. Borrowed as a fixed slice so
/// the orchestration in each handler passes them through unchanged.
///
/// | slot | account                  | role                                   |
/// |------|--------------------------|----------------------------------------|
/// | 0    | relayer_registration_pda | relayer-emitter registry for emitter_chain (read) |
/// | 1    | transceiver_hub_pda      | hub mapping `(emitter_chain, sender)` (read) |
/// | 2    | transceiver_peer_src_pda | source→dest peer `(emitter_chain, sender, recipient_chain)` (read) |
/// | 3    | transceiver_peer_dst_pda | dest→source peer `(recipient_chain, source_peer, emitter_chain)` (read) |
/// | 4    | source_balance           | `(emitter_chain, hub_chain, hub_address)` (write, lazy-init) |
/// | 5    | dest_balance             | `(recipient_chain, hub_chain, hub_address)` (write, lazy-init) |
pub const TRANSFER_ACCOUNTS_LEN: usize = 6;

/// Run the NTT transfer flow for a verified VAA body. `payer` funds lazy-inits.
/// `transfer_accounts` is the 6-element slice documented on
/// [`TRANSFER_ACCOUNTS_LEN`]. `emitter_chain` / `emitter_address` are the
/// authenticated routing key from the body header (NOT the relayer-resolved
/// sender — that distinction is load-bearing: the digest/transfer key is the
/// emitter, the hub/peer routing uses the sender).
pub fn apply_ntt_transfer(
    program_id: &Address,
    payer: &mut AccountView,
    transfer_accounts: &mut [AccountView],
    emitter_chain: u16,
    emitter_address: &[u8; 32],
    body_bytes: &[u8],
) -> ProgramResult {
    let [relayer_registration_pda, transceiver_hub_pda, transceiver_peer_src_pda, transceiver_peer_dst_pda, source_balance, dest_balance] =
        transfer_accounts
    else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    if body_bytes.len() <= VAA_BODY_HEADER_LEN {
        return Err(err(GlobalAccountantError::InvalidInstructionData));
    }
    let body_payload = &body_bytes[VAA_BODY_HEADER_LEN..];

    // ----- (1) Relayer-unwrap -----
    //
    // If the registered relayer for `emitter_chain` is exactly `emitter_address`,
    // the payload is a `DeliveryInstruction` wrapping the real sender + the NTT
    // message; unwrap it. Otherwise the emitter is itself the sender and the
    // payload is the NTT message directly.
    let (sender, ntt_payload): ([u8; 32], &[u8]) = if relayer_matches(
        program_id,
        relayer_registration_pda,
        emitter_chain,
        emitter_address,
    )? {
        let unwrap = parse_delivery_instruction(body_payload).map_err(err)?;
        (unwrap.sender, unwrap.inner_payload)
    } else {
        (*emitter_address, body_payload)
    };

    // ----- (2) Hub-substitute -----
    //
    // The accounted token identity is the hub's `(hub_chain, hub_address)`, read
    // from the TransceiverHub PDA keyed by the routing `(emitter_chain, sender)`.
    let hub = read_transceiver_hub(program_id, transceiver_hub_pda, emitter_chain, &sender)?;
    let (hub_chain, hub_address) = (hub.hub_chain, hub.hub_address);

    // ----- (3) Parse the NTT transfer -----
    let transfer = parse_ntt_transfer(ntt_payload).map_err(err)?;
    let amount = transfer.amount;
    let recipient_chain = transfer.recipient_chain;

    // ----- (4) Peer cross-registration check -----
    //
    // Source→dest: TransceiverPeer at `(emitter_chain, sender, recipient_chain)`
    // gives the registered peer on the recipient chain. Dest→source: the reverse
    // PDA at `(recipient_chain, source_peer, emitter_chain)` must register back
    // to `sender` — both directions must agree before accounting.
    let source_peer = read_transceiver_peer(
        program_id,
        transceiver_peer_src_pda,
        emitter_chain,
        &sender,
        recipient_chain,
    )?;
    let reverse_peer = read_transceiver_peer(
        program_id,
        transceiver_peer_dst_pda,
        recipient_chain,
        &source_peer.peer_address,
        emitter_chain,
    )?;
    if reverse_peer.peer_address != sender {
        return Err(err(GlobalAccountantError::PeerRegistrationMismatch));
    }

    // ----- (5) Apply accounting against the HUB token identity -----
    apply_balances(
        program_id,
        payer,
        source_balance,
        dest_balance,
        emitter_chain,
        recipient_chain,
        hub_chain,
        &hub_address,
        amount,
    )
}

/// True if the relayer-registration PDA for `chain` is initialised and registers
/// exactly `emitter_address`. A missing (system-owned) PDA, or one registering a
/// different emitter, means the emitter is not a relayer — `false`. The PDA
/// address is canonical-checked first.
fn relayer_matches(
    program_id: &Address,
    relayer_registration_pda: &AccountView,
    chain: u16,
    emitter_address: &[u8; 32],
) -> Result<bool, ProgramError> {
    let chain_be = chain.to_be_bytes();
    let (expected, _) = Address::find_program_address(
        &[RELAYER_CHAIN_REGISTRATION_SEED_PREFIX, &chain_be],
        program_id,
    );
    if relayer_registration_pda.address() != &expected {
        return Err(err(GlobalAccountantError::InvalidPda));
    }
    // Unregistered chain ⇒ not a relayer.
    if relayer_registration_pda.owner() == &pinocchio_system::ID {
        return Ok(false);
    }
    let data: Ref<'_, [u8]> = relayer_registration_pda.try_borrow()?;
    if data.len() != RelayerChainRegistrationLayout::LEN {
        return Err(err(GlobalAccountantError::InvalidPda));
    }
    let layout = bytemuck::from_bytes::<RelayerChainRegistrationLayout>(&data);
    if layout.tag != RelayerChainRegistrationLayout::TAG {
        return Err(err(GlobalAccountantError::InvalidPda));
    }
    Ok(&layout.emitter_address == emitter_address)
}

/// Read the `TransceiverHub` PDA at `(b"transceiver_hub", chain_be, address)`.
/// Canonical-address-checked; a missing PDA is `MissingTransceiverHub`.
fn read_transceiver_hub(
    program_id: &Address,
    hub_pda: &AccountView,
    chain: u16,
    address: &[u8; 32],
) -> Result<TransceiverHubLayout, ProgramError> {
    let chain_be = chain.to_be_bytes();
    let (expected, _) = Address::find_program_address(
        &[TRANSCEIVER_HUB_SEED_PREFIX, &chain_be, address],
        program_id,
    );
    if hub_pda.address() != &expected {
        return Err(err(GlobalAccountantError::InvalidPda));
    }
    if hub_pda.owner() == &pinocchio_system::ID {
        return Err(err(GlobalAccountantError::MissingTransceiverHub));
    }
    let data: Ref<'_, [u8]> = hub_pda.try_borrow()?;
    if data.len() != TransceiverHubLayout::LEN {
        return Err(err(GlobalAccountantError::InvalidPda));
    }
    let layout = bytemuck::from_bytes::<TransceiverHubLayout>(&data);
    if layout.tag != TransceiverHubLayout::TAG {
        return Err(err(GlobalAccountantError::InvalidPda));
    }
    Ok(*layout)
}

/// Read the `TransceiverPeer` PDA at `(b"transceiver_peer", chain_be, address,
/// dest_chain_be)`. Canonical-address-checked; a missing PDA is
/// `MissingTransceiverPeer`.
fn read_transceiver_peer(
    program_id: &Address,
    peer_pda: &AccountView,
    chain: u16,
    address: &[u8; 32],
    dest_chain: u16,
) -> Result<TransceiverPeerLayout, ProgramError> {
    let chain_be = chain.to_be_bytes();
    let dest_chain_be = dest_chain.to_be_bytes();
    let (expected, _) = Address::find_program_address(
        &[
            TRANSCEIVER_PEER_SEED_PREFIX,
            &chain_be,
            address,
            &dest_chain_be,
        ],
        program_id,
    );
    if peer_pda.address() != &expected {
        return Err(err(GlobalAccountantError::InvalidPda));
    }
    if peer_pda.owner() == &pinocchio_system::ID {
        return Err(err(GlobalAccountantError::MissingTransceiverPeer));
    }
    let data: Ref<'_, [u8]> = peer_pda.try_borrow()?;
    if data.len() != TransceiverPeerLayout::LEN {
        return Err(err(GlobalAccountantError::InvalidPda));
    }
    let layout = bytemuck::from_bytes::<TransceiverPeerLayout>(&data);
    if layout.tag != TransceiverPeerLayout::TAG {
        return Err(err(GlobalAccountantError::InvalidPda));
    }
    Ok(*layout)
}

/// Mutate the source / destination balance account PDAs for the transfer,
/// keyed by the HUB token identity `(hub_chain, hub_address)`. Source side
/// `lock_or_burn`, destination side `unlock_or_mint`. Same-chain self-transfers
/// (source == dest PDA) collapse onto one in-memory layout so the second
/// mutation observes the first. Identical mechanics to WTT `transfer::apply_transfer`.
#[allow(clippy::too_many_arguments)]
fn apply_balances(
    program_id: &Address,
    payer: &AccountView,
    source_account: &mut AccountView,
    dest_account: &mut AccountView,
    source_chain: u16,
    recipient_chain: u16,
    token_chain: u16,
    token_address: &[u8; 32],
    amount: Uint256,
) -> ProgramResult {
    // ----- Source side -----
    let (src_expected, src_bump) =
        derive_balance_account_pda(program_id, source_chain, token_chain, token_address);
    if source_account.address() != &src_expected {
        return Err(err(GlobalAccountantError::InvalidAccountPda));
    }
    account_state::init_if_needed(
        program_id,
        payer,
        source_account,
        source_chain,
        token_chain,
        token_address,
        src_bump,
    )?;
    let mut src = account_state::load(source_account)?;
    src.lock_or_burn(amount).map_err(err)?;

    let same_pda = source_account.address() == dest_account.address();
    if same_pda {
        src.unlock_or_mint(amount).map_err(err)?;
        account_state::store(source_account, &src)?;
        return Ok(());
    }

    account_state::store(source_account, &src)?;

    // ----- Destination side -----
    let (dst_expected, dst_bump) =
        derive_balance_account_pda(program_id, recipient_chain, token_chain, token_address);
    if dest_account.address() != &dst_expected {
        return Err(err(GlobalAccountantError::InvalidAccountPda));
    }
    account_state::init_if_needed(
        program_id,
        payer,
        dest_account,
        recipient_chain,
        token_chain,
        token_address,
        dst_bump,
    )?;
    let mut dst = account_state::load(dest_account)?;
    dst.unlock_or_mint(amount).map_err(err)?;
    account_state::store(dest_account, &dst)
}

/// Re-derive the canonical balance account PDA address + bump from `(chain,
/// token_chain, token_address)`. Identical seed layout to WTT.
fn derive_balance_account_pda(
    program_id: &Address,
    chain: u16,
    token_chain: u16,
    token_address: &[u8; 32],
) -> (Address, u8) {
    let chain_be = chain.to_be_bytes();
    let token_chain_be = token_chain.to_be_bytes();
    Address::find_program_address(
        &[
            ACCOUNT_SEED_PREFIX,
            &chain_be,
            &token_chain_be,
            token_address,
        ],
        program_id,
    )
}
