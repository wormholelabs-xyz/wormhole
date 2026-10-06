//! Token Bridge bodies, observation ix data, and WTT-discriminator framing wrappers.

use accountant_operational_core::support::quorum::{observation_digests, ObservationDigests};
use accountant_test_harness::wire;
use accountant_test_harness::{double_keccak256, vaa_header, TX_ID};
use global_accountant_definitions::{
    parse_token_bridge_payload, Instruction, SubmitObservationsIxData, TokenBridgeAction,
    TokenBridgeTransfer, TxId, Uint256, VaaBodyHeader, ACTION_ATTEST, ACTION_TRANSFER,
    ACTION_TRANSFER_WITH_PAYLOAD, SIGNATURE_TX_ID_LEN, SUBMIT_OBSERVATION_PREFIX,
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

/// Builds the `SubmitObservationsIxData` a guardian would send for `body`.
pub fn observation_ix_from_body(
    guardian_set_index: u32,
    guardian_index: u8,
    signature: [u8; 65],
    tx_id: TxId<'_>,
    body: &[u8],
) -> SubmitObservationsIxData {
    let tx_id_bytes = tx_id.as_bytes();
    let mut tx_id_padded = [0u8; SIGNATURE_TX_ID_LEN];
    tx_id_padded[..tx_id_bytes.len()].copy_from_slice(tx_id_bytes);
    let (header, payload) = VaaBodyHeader::split(body).expect("test body has a valid VAA header");
    let key = header.namespace_key();
    let action_byte = *payload.first().expect("test body has a non-empty payload");
    let (token_chain, token_address, recipient_chain, amount) =
        match parse_token_bridge_payload(body).expect("test body has a valid token bridge payload")
        {
            TokenBridgeAction::Transfer {
                amount,
                token_chain,
                token_address,
                recipient_chain,
            } => (token_chain, token_address, recipient_chain, amount),
            TokenBridgeAction::Attest | TokenBridgeAction::Other(_) => {
                (0, [0u8; 32], 0, Uint256::ZERO)
            }
        };
    SubmitObservationsIxData {
        guardian_set_index: guardian_set_index.to_le_bytes(),
        guardian_index,
        signature,
        tx_id_len: u8::try_from(tx_id_bytes.len()).expect("tx id length fits u8"),
        tx_id: tx_id_padded,
        action: action_byte,
        chain: key.chain.to_be_bytes(),
        emitter: key.emitter,
        sequence: key.sequence.to_be_bytes(),
        token_chain: token_chain.to_be_bytes(),
        token_address,
        recipient_chain: recipient_chain.to_be_bytes(),
        amount,
        digest: double_keccak256(body),
    }
}

fn digests_with_tx_id(tx_id: TxId<'_>, body: &[u8]) -> ObservationDigests {
    let ix = observation_ix_from_body(0, 0, [0u8; 65], tx_id, body);
    observation_digests(SUBMIT_OBSERVATION_PREFIX, tx_id, &ix.fields_and_digest())
}

pub fn content_digest(body: &[u8]) -> [u8; 32] {
    digests_with_tx_id(TX_ID, body).content
}

pub fn signing_digest(body: &[u8]) -> [u8; 32] {
    signing_digest_with_tx_id(TX_ID, body)
}

pub fn signing_digest_with_tx_id(tx_id: TxId<'_>, body: &[u8]) -> [u8; 32] {
    digests_with_tx_id(tx_id, body).signing
}

pub fn submit_observations_ix_data(
    guardian_set_index: u32,
    guardian_index: u8,
    signature: [u8; 65],
    body: &[u8],
) -> Vec<u8> {
    submit_observations_ix_data_with_tx_id(
        guardian_set_index,
        guardian_index,
        signature,
        TX_ID,
        body,
    )
}

pub fn submit_observations_ix_data_with_tx_id(
    guardian_set_index: u32,
    guardian_index: u8,
    signature: [u8; 65],
    tx_id: TxId<'_>,
    body: &[u8],
) -> Vec<u8> {
    let ix = observation_ix_from_body(guardian_set_index, guardian_index, signature, tx_id, body);
    wire::framed(
        Instruction::SubmitObservations as u8,
        bytemuck::bytes_of(&ix),
        &[],
    )
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
