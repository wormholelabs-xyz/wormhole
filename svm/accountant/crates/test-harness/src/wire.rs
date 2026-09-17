//! Instruction-data framing for the shared handlers: `discriminator ‖ prefix ‖ body`.
//! Each program passes its own discriminator byte.

use global_accountant_definitions::{
    ClosePendingIxData, ModifyBalanceIxData, RegisterChainIxData, SubmitVaasIxData,
    UpgradeContractIxData,
};

/// `prefix` is a `Pod` `*IxData` struct as raw bytes; its `body_len` field must equal
/// `body.len()`. `split_body` reverses this layout and rejects any other length.
pub fn framed(discriminator: u8, prefix: &[u8], body: &[u8]) -> Vec<u8> {
    let mut data = Vec::with_capacity(1 + prefix.len() + body.len());
    data.push(discriminator);
    data.extend_from_slice(prefix);
    data.extend_from_slice(body);
    data
}

pub fn submit_vaas(discriminator: u8, guardian_set_bump: u8, body: &[u8]) -> Vec<u8> {
    submit_vaas_with_len(discriminator, guardian_set_bump, body.len() as u16, body)
}

pub fn submit_vaas_with_len(
    discriminator: u8,
    guardian_set_bump: u8,
    body_len: u16,
    body: &[u8],
) -> Vec<u8> {
    let prefix = SubmitVaasIxData {
        guardian_set_bump,
        body_len: body_len.to_le_bytes(),
    };
    framed(discriminator, bytemuck::bytes_of(&prefix), body)
}

pub fn register_chain(discriminator: u8, guardian_set_bump: u8, body: &[u8]) -> Vec<u8> {
    let prefix = RegisterChainIxData {
        guardian_set_bump,
        body_len: (body.len() as u16).to_le_bytes(),
    };
    framed(discriminator, bytemuck::bytes_of(&prefix), body)
}

pub fn modify_balance(discriminator: u8, guardian_set_bump: u8, body: &[u8]) -> Vec<u8> {
    let prefix = ModifyBalanceIxData {
        guardian_set_bump,
        body_len: (body.len() as u16).to_le_bytes(),
    };
    framed(discriminator, bytemuck::bytes_of(&prefix), body)
}

pub fn upgrade_contract(discriminator: u8, guardian_set_bump: u8, body: &[u8]) -> Vec<u8> {
    let prefix = UpgradeContractIxData {
        guardian_set_bump,
        body_len: (body.len() as u16).to_le_bytes(),
    };
    framed(discriminator, bytemuck::bytes_of(&prefix), body)
}

pub fn close_pending(discriminator: u8, emitter: [u8; 32], sequence: u64) -> Vec<u8> {
    let data = ClosePendingIxData {
        emitter,
        sequence: sequence.to_be_bytes(),
    };
    framed(discriminator, bytemuck::bytes_of(&data), &[])
}
