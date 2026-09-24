//! Instruction-data framing for the shared handlers: `discriminator ‖ prefix ‖ body`.
//! Each program passes its own discriminator byte.

use global_accountant_definitions::{
    ClosePendingIxData, DeliveryHead, DeliveryMiddle, DeliveryTail, ModifyBalanceIxData,
    RegisterChainIxData, SubmitVaasIxData, UpgradeContractIxData, DELIVERY_INSTRUCTION_PAYLOAD_ID,
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

/// Standard Relayer `DeliveryInstruction` from `sender` wrapping `payload`, with no
/// execution info and no message keys.
pub fn delivery_instruction(sender: [u8; 32], payload: &[u8]) -> Vec<u8> {
    let head = DeliveryHead {
        payload_id: DELIVERY_INSTRUCTION_PAYLOAD_ID,
        target_chain: 1u16.to_be_bytes(),
        target_address: [0x01; 32],
        payload_len: (payload.len() as u32).to_be_bytes(),
    };
    let middle = DeliveryMiddle {
        requested_reciever_value: [0; 32],
        extra_reciever_value: [0; 32],
        exec_info_len: [0; 4],
    };
    let tail = DeliveryTail {
        refund_chain: 1u16.to_be_bytes(),
        refund_address: [0x04; 32],
        refund_delivery_provider: [0x05; 32],
        source_delivery_provider: [0x06; 32],
        sender_address: sender,
        num_messages: 0,
    };
    [
        bytemuck::bytes_of(&head),
        payload,
        bytemuck::bytes_of(&middle),
        bytemuck::bytes_of(&tail),
    ]
    .concat()
}
