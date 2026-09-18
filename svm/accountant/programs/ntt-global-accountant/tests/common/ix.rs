//! Instruction-data framing bound to the NTT discriminators.

use accountant_test_harness::wire;
use global_accountant_definitions::instructions::ntt_global_accountant::Instruction;

pub fn close_pending_ix_data(emitter: [u8; 32], sequence: u64) -> Vec<u8> {
    wire::close_pending(Instruction::ClosePending as u8, emitter, sequence)
}

pub fn register_relayer_chain_ix_data(guardian_set_bump: u8, body: &[u8]) -> Vec<u8> {
    wire::register_chain(
        Instruction::RegisterRelayerChain as u8,
        guardian_set_bump,
        body,
    )
}

pub fn modify_balance_ix_data(guardian_set_bump: u8, body: &[u8]) -> Vec<u8> {
    wire::modify_balance(Instruction::ModifyBalance as u8, guardian_set_bump, body)
}

pub fn upgrade_contract_ix_data(guardian_set_bump: u8, body: &[u8]) -> Vec<u8> {
    wire::upgrade_contract(Instruction::UpgradeContract as u8, guardian_set_bump, body)
}
