//! NTT bodies and instruction-data framing bound to the NTT discriminators.

use std::mem::size_of;

use accountant_test_harness::{vaa_header, wire};
pub use global_accountant_definitions::instructions::ntt_global_accountant::Instruction as NttInstruction;
use global_accountant_definitions::instructions::ntt_global_accountant::Instruction;
use global_accountant_definitions::{
    ManagerHead, ManagerMode, NativeTokenTransfer, RegisterHubIxData, RegisterPeerIxData,
    TransceiverHead, TransceiverInfoPayload, TransceiverRegistrationPayload,
    NATIVE_TOKEN_TRANSFER_PREFIX, TRANSCEIVER_INFO_PREFIX, TRANSCEIVER_MESSAGE_PREFIX,
    TRANSCEIVER_PEER_INFO_PREFIX,
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
