//! Token Bridge payload dispatch onto the shared balance applier, used by `submit_vaas`.

use anchor_lang::prelude::*;

use crate::definitions::{parse_token_bridge_payload, GlobalAccountantError, TokenBridgeAction};
use crate::err;
use accountant_operational_core::{transfer, ProgramResult};

/// Parse the Token Bridge payload and apply it. Attest is a no-op; an unknown action
/// fails the transaction, which rolls back the NoReplay mark.
pub fn apply_from_body<'info>(
    program_id: &Pubkey,
    submitter: &AccountInfo<'info>,
    source_account_pda: &AccountInfo<'info>,
    dest_account_pda: &AccountInfo<'info>,
    source_chain: u16,
    body_bytes: &[u8],
) -> ProgramResult {
    match parse_token_bridge_payload(body_bytes).map_err(err)? {
        TokenBridgeAction::Transfer {
            amount,
            token_chain,
            token_address,
            recipient_chain,
        } => transfer::apply_transfer(
            program_id,
            submitter,
            source_account_pda,
            dest_account_pda,
            source_chain,
            recipient_chain,
            token_chain,
            &token_address,
            amount,
        ),
        TokenBridgeAction::Attest => Ok(()),
        TokenBridgeAction::Other(_) => Err(err(GlobalAccountantError::UnknownTokenBridgePayload)),
    }
}
