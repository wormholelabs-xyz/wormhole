//! NTT bodies and instruction-data framing bound to the NTT discriminators.

use accountant_test_harness::{vaa_header, wire};
use global_accountant_definitions::instructions::ntt_global_accountant::Instruction;
use global_accountant_definitions::{
    ManagerMode, RegisterHubIxData, RegisterPeerIxData, TransceiverInfoPayload,
    TransceiverRegistrationPayload, TRANSCEIVER_INFO_PREFIX, TRANSCEIVER_PEER_INFO_PREFIX,
};

/// `WormholeTransceiverInfo` payload in `mode`.
pub fn hub_payload(mode: ManagerMode) -> Vec<u8> {
    let info = TransceiverInfoPayload {
        prefix: TRANSCEIVER_INFO_PREFIX,
        manager_address: [0x11; 32],
        mode: mode as u8,
        token_address: [0x22; 32],
        token_decimals: 8,
    };
    bytemuck::bytes_of(&info).to_vec()
}

/// `WormholeTransceiverRegistration` payload for `peer` on `dest_chain`.
pub fn peer_payload(dest_chain: u16, peer: [u8; 32]) -> Vec<u8> {
    let registration = TransceiverRegistrationPayload {
        prefix: TRANSCEIVER_PEER_INFO_PREFIX,
        chain: dest_chain.to_be_bytes(),
        transceiver_address: peer,
    };
    bytemuck::bytes_of(&registration).to_vec()
}

/// VAA body published directly by `emitter` on `chain`.
pub fn direct_body(chain: u16, emitter: [u8; 32], sequence: u64, payload: &[u8]) -> Vec<u8> {
    let mut body = vaa_header(chain, emitter, sequence);
    body.extend_from_slice(payload);
    body
}

/// VAA body published by `relayer` on `chain`, wrapping `payload` from `sender`.
pub fn relayed_body(
    chain: u16,
    relayer: [u8; 32],
    sequence: u64,
    sender: [u8; 32],
    payload: &[u8],
) -> Vec<u8> {
    direct_body(
        chain,
        relayer,
        sequence,
        &wire::delivery_instruction(sender, payload),
    )
}

pub fn register_hub_ix_data(guardian_set_bump: u8, body: &[u8]) -> Vec<u8> {
    let prefix = RegisterHubIxData {
        guardian_set_bump,
        body_len: (body.len() as u16).to_le_bytes(),
    };
    wire::framed(
        Instruction::RegisterHub as u8,
        bytemuck::bytes_of(&prefix),
        body,
    )
}

pub fn register_peer_ix_data(guardian_set_bump: u8, body: &[u8]) -> Vec<u8> {
    let prefix = RegisterPeerIxData {
        guardian_set_bump,
        body_len: (body.len() as u16).to_le_bytes(),
    };
    wire::framed(
        Instruction::RegisterPeer as u8,
        bytemuck::bytes_of(&prefix),
        body,
    )
}

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
