//! Token Bridge bodies and WTT-discriminator framing wrappers.

use accountant_test_harness::vaa_header;
use accountant_test_harness::wire;
use global_accountant_definitions::{
    Instruction, TokenBridgeTransfer, Uint256, VaaBodyHeader, ACTION_ATTEST, ACTION_TRANSFER,
    ACTION_TRANSFER_WITH_PAYLOAD,
};

pub const RECIPIENT: [u8; 32] = [0xAB; 32];

pub fn attest_body(emitter_chain: u16, emitter_address: [u8; 32], sequence: u64) -> Vec<u8> {
    let mut body = vaa_header(emitter_chain, emitter_address, sequence);
    body.push(ACTION_ATTEST);
    body
}

pub fn transfer_body(
    emitter_chain: u16,
    emitter_address: [u8; 32],
    sequence: u64,
    amount: Uint256,
    token_chain: u16,
    token_address: [u8; 32],
    recipient_chain: u16,
) -> Vec<u8> {
    let transfer = TokenBridgeTransfer::new(
        ACTION_TRANSFER,
        amount,
        token_address,
        token_chain,
        RECIPIENT,
        recipient_chain,
        Uint256::ZERO,
    );
    let mut body = vaa_header(emitter_chain, emitter_address, sequence);
    body.extend_from_slice(bytemuck::bytes_of(&transfer));
    body
}

/// Turns a `transfer_body` into action `0x03` with `payload` appended. The fee field
/// becomes `from_address`.
pub fn with_transfer_payload(mut body: Vec<u8>, payload: &[u8]) -> Vec<u8> {
    assert_eq!(
        body.len(),
        VaaBodyHeader::LEN + TokenBridgeTransfer::LEN,
        "input is a bare transfer body"
    );
    assert_eq!(body[VaaBodyHeader::LEN], ACTION_TRANSFER, "input action");
    body[VaaBodyHeader::LEN] = ACTION_TRANSFER_WITH_PAYLOAD;
    body.extend_from_slice(payload);
    body
}

pub fn submit_vaas_ix_data(guardian_set_bump: u8, body: &[u8]) -> Vec<u8> {
    wire::submit_vaas(Instruction::SubmitVaas as u8, guardian_set_bump, body)
}

pub fn submit_vaas_ix_data_with_len(guardian_set_bump: u8, body_len: u16, body: &[u8]) -> Vec<u8> {
    wire::submit_vaas_with_len(
        Instruction::SubmitVaas as u8,
        guardian_set_bump,
        body_len,
        body,
    )
}

pub fn register_chain_ix_data(guardian_set_bump: u8, body: &[u8]) -> Vec<u8> {
    wire::register_chain(Instruction::RegisterChain as u8, guardian_set_bump, body)
}

pub fn modify_balance_ix_data(guardian_set_bump: u8, body: &[u8]) -> Vec<u8> {
    wire::modify_balance(Instruction::ModifyBalance as u8, guardian_set_bump, body)
}

pub fn upgrade_contract_ix_data(guardian_set_bump: u8, body: &[u8]) -> Vec<u8> {
    wire::upgrade_contract(Instruction::UpgradeContract as u8, guardian_set_bump, body)
}

pub fn close_pending_ix_data(emitter: [u8; 32], sequence: u64) -> Vec<u8> {
    wire::close_pending(Instruction::ClosePending as u8, emitter, sequence)
}
