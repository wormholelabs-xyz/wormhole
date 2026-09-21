//! Constants, key helpers, the signed-VAA scaffold and result assertions shared by scenario
//! builders.

use std::ops::Range;

use mollusk_svm::result::{InstructionResult, ProgramResult};
use mollusk_svm::Mollusk;
use solana_account::Account;
use solana_instruction::{AccountMeta, Instruction};
use solana_pubkey::Pubkey;

use crate::guardians::{
    derive_guardian_set_pda, guardian_set_account, guardian_signatures_account, make_guardians,
    sign_digest, Guardian, GUARDIAN_PUBKEY_LENGTH,
};
use crate::ids::{core_bridge_program_id, shim_program_id};
use crate::ix::double_keccak256;
use crate::mollusk::{keyed_account_for_verify_vaa_shim_program, system_owned_account};

pub const GUARDIAN_COUNT: usize = 19;
pub const QUORUM: u8 = 13;
pub const GUARDIAN_SET_INDEX: u32 = 4;
pub const SOLANA: u16 = 1;
pub const ETHEREUM: u16 = 2;
pub const SUBMITTER: Pubkey = Pubkey::new_from_array([0x11u8; 32]);
pub const GUARDIAN_SIGNATURES: Pubkey = Pubkey::new_from_array([0xC5u8; 32]);

pub fn emitter(seed: u8) -> [u8; 32] {
    let mut emitter = [0u8; 32];
    emitter[0] = seed;
    emitter[31] = 0x77;
    emitter
}

pub fn error_code(result: &ProgramResult) -> Option<u64> {
    match result {
        ProgramResult::Failure(err) => Some(u64::from(err.clone())),
        _ => None,
    }
}

pub fn assert_success(result: &InstructionResult, label: &str) {
    assert!(
        matches!(result.program_result, ProgramResult::Success),
        "{label}: {:?}",
        result.program_result
    );
}

pub fn assert_error(result: &InstructionResult, expected: u64, label: &str) {
    assert_eq!(
        error_code(&result.program_result),
        Some(expected),
        "{label}: {:?}",
        result.program_result
    );
}

pub fn signatures_for(guardians: &[Guardian], digest: &[u8; 32], count: u8) -> Vec<(u8, [u8; 65])> {
    (0..count)
        .map(|i| (i, sign_digest(&guardians[i as usize], digest)))
        .collect()
}

pub fn guardian_keys(guardians: &[Guardian]) -> Vec<[u8; GUARDIAN_PUBKEY_LENGTH]> {
    guardians.iter().map(|g| g.eth_address).collect()
}

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

/// Submit one observation per guardian index in `range`, each on the previous result's
/// accounts. Every submission must succeed.
pub fn submit_range(
    mollusk: &Mollusk,
    mut accounts: Vec<(Pubkey, Account)>,
    range: Range<u8>,
    submit_once: impl Fn(&Mollusk, Vec<(Pubkey, Account)>, u8) -> InstructionResult,
) -> Vec<(Pubkey, Account)> {
    for i in range {
        let result = submit_once(mollusk, accounts, i);
        assert_success(&result, &format!("observation {i}"));
        accounts = result.resulting_accounts;
    }
    accounts
}
