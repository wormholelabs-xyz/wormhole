use crate::{error::WormholeVerifyVaaShim, state::GuardianSignatures};
use anchor_lang::prelude::*;

#[derive(Accounts)]
#[instruction(_guardian_set_index: u32, total_signatures: u8, _guardian_signatures: Vec<[u8; 66]>)]
pub struct PostSignatures<'info> {
    #[account(mut)]
    payer: Signer<'info>,

    #[account(
        init_if_needed,
        payer = payer,
        space = 8 + GuardianSignatures::compute_size(usize::from(total_signatures))
    )]
    guardian_signatures: Account<'info, GuardianSignatures>,

    system_program: Program<'info, System>,
}

impl<'info> PostSignatures<'info> {
    pub fn constraints(guardian_signatures: &Vec<[u8; 66]>) -> Result<()> {
        // Signatures should not be empty, since this is used by is_initialized.
        // Additionally, there is no reason for it to be.
        require!(
            !guardian_signatures.is_empty(),
            WormholeVerifyVaaShim::EmptyGuardianSignatures
        );

        // Done.
        Ok(())
    }
}

#[access_control(PostSignatures::constraints(&guardian_signatures))]
pub fn post_signatures(
    ctx: Context<PostSignatures>,
    guardian_set_index: u32,
    _total_signatures: u8,
    mut guardian_signatures: Vec<[u8; 66]>,
) -> Result<()> {
    if ctx.accounts.guardian_signatures.is_initialized() {
        require_eq!(
            ctx.accounts.guardian_signatures.refund_recipient,
            ctx.accounts.payer.key(),
            WormholeVerifyVaaShim::WriteAuthorityMismatch
        );
        ctx.accounts
            .guardian_signatures
            .guardian_signatures
            .append(&mut guardian_signatures);
    } else {
        ctx.accounts
            .guardian_signatures
            .set_inner(GuardianSignatures {
                refund_recipient: ctx.accounts.payer.key(),
                guardian_set_index_be: guardian_set_index.to_be_bytes(),
                guardian_signatures,
            });
    }
    // Done.
    Ok(())
}