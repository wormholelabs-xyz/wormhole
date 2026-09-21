use mollusk_svm::result::InstructionResult;
use mollusk_svm::Mollusk;
use solana_account::Account;
use solana_instruction::{AccountMeta, Instruction};
use solana_pubkey::Pubkey;

use super::*;

/// A VAA body signed by the test guardian set: the four Shim-facing accounts every
/// Shim-verified instruction starts with (payer, Shim program, guardian set, signatures).
#[derive(Clone)]
pub struct SignedVaa {
    pub body: Vec<u8>,
    pub guardian_set: Pubkey,
    pub guardian_set_bump: u8,
    guardians: Vec<Guardian>,
}

impl SignedVaa {
    pub fn new(body: Vec<u8>) -> Self {
        let (guardian_set, guardian_set_bump) =
            derive_guardian_set_pda(GUARDIAN_SET_INDEX, &core_bridge_program_id());
        Self {
            body,
            guardian_set,
            guardian_set_bump,
            guardians: make_guardians(GUARDIAN_COUNT, 0x42),
        }
    }

    pub fn shim_metas(&self) -> Vec<AccountMeta> {
        vec![
            AccountMeta::new(SUBMITTER, true),
            AccountMeta::new_readonly(shim_program_id(), false),
            AccountMeta::new_readonly(self.guardian_set, false),
            AccountMeta::new_readonly(GUARDIAN_SIGNATURES, false),
        ]
    }

    pub fn shim_accounts(&self) -> Vec<(Pubkey, Account)> {
        let digest = double_keccak256(&self.body);
        vec![
            (SUBMITTER, system_owned_account(50_000_000_000)),
            keyed_account_for_verify_vaa_shim_program(),
            (
                self.guardian_set,
                guardian_set_account(
                    GUARDIAN_SET_INDEX,
                    &guardian_keys(&self.guardians),
                    0,
                    0,
                    &core_bridge_program_id(),
                ),
            ),
            (
                GUARDIAN_SIGNATURES,
                guardian_signatures_account(
                    GUARDIAN_SET_INDEX,
                    &SUBMITTER,
                    &signatures_for(&self.guardians, &digest, QUORUM),
                    &shim_program_id(),
                ),
            ),
        ]
    }

    /// Run `ix_data` against `program` with the Shim accounts first, then `extra`.
    pub fn submit(
        &self,
        mollusk: &Mollusk,
        program: Pubkey,
        ix_data: &[u8],
        extra_metas: Vec<AccountMeta>,
        extra_accounts: Vec<(Pubkey, Account)>,
    ) -> InstructionResult {
        let mut metas = self.shim_metas();
        metas.extend(extra_metas);
        let mut accounts = self.shim_accounts();
        accounts.extend(extra_accounts);
        let ix = Instruction::new_with_bytes(program, ix_data, metas);
        mollusk.process_instruction(&ix, &accounts)
    }
}
