//! Constants, key helpers and result assertions shared by scenario builders.

use mollusk_svm::result::{InstructionResult, ProgramResult};
use solana_pubkey::Pubkey;

use crate::guardians::{sign_digest, Guardian, GUARDIAN_PUBKEY_LENGTH};

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
