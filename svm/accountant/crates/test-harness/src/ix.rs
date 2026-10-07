//! VAA body builders for governance payloads, WTT observation ix data, and instructions of
//! sibling programs.

use accountant_operational_core::support::quorum::{observation_digests, ObservationDigests};
use global_accountant_definitions::{
    parse_token_bridge_payload, GovernanceHeader, GovernanceModule, Instruction,
    ModifyBalancePayload, PostSignaturesIxData, RegisterChainPayload, SubmitObservationsIxData,
    TokenBridgeAction, TxId, Uint256, UpgradeContractPayload, VaaBodyHeader, SIGNATURE_TX_ID_LEN,
    SUBMIT_OBSERVATION_PREFIX,
};
use solana_instruction::{AccountMeta, Instruction as SvmInstruction};
use solana_pubkey::Pubkey;

use crate::guardians::GUARDIAN_SIGNATURE_LENGTH;
use crate::ids::{shim_program_id, system_program_id};
use crate::wire;

pub use accountant_operational_core::hash::double_keccak256;

pub const TX_ID: TxId<'static> = TxId::Hash(&[0xA9u8; 32]);

pub fn vaa_header(emitter_chain: u16, emitter_address: [u8; 32], sequence: u64) -> Vec<u8> {
    let header = VaaBodyHeader::new(0, 0, emitter_chain, emitter_address, sequence, 0);
    bytemuck::bytes_of(&header).to_vec()
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

/// Near-miss modules for rejection tests.
pub trait GovernanceModuleExt {
    /// `self` with the lowest bit of its last name byte flipped: one bit away from a valid
    /// module. Proves the handler compares all 32 module bytes for exact equality.
    fn one_bit_off(self) -> Self;
}

impl GovernanceModuleExt for GovernanceModule {
    fn one_bit_off(mut self) -> Self {
        self.0[31] ^= 1;
        self
    }
}

pub fn governance_header(
    module: GovernanceModule,
    action: u8,
    target_chain: u16,
) -> GovernanceHeader {
    GovernanceHeader {
        module,
        action,
        target_chain: target_chain.to_be_bytes(),
    }
}

pub fn register_chain_body(
    emitter_chain: u16,
    emitter_address: [u8; 32],
    sequence: u64,
    header: GovernanceHeader,
    chain: u16,
    chain_emitter: [u8; 32],
) -> Vec<u8> {
    let payload = RegisterChainPayload {
        header,
        chain: chain.to_be_bytes(),
        emitter_address: chain_emitter,
    };
    let mut body = vaa_header(emitter_chain, emitter_address, sequence);
    body.extend_from_slice(bytemuck::bytes_of(&payload));
    body
}

#[allow(clippy::too_many_arguments)]
pub fn modify_balance_body(
    emitter_chain: u16,
    emitter_address: [u8; 32],
    sequence: u64,
    header: GovernanceHeader,
    payload_sequence: u64,
    chain_id: u16,
    token_chain: u16,
    token_address: [u8; 32],
    kind: u8,
    amount: Uint256,
    reason: [u8; 32],
) -> Vec<u8> {
    let payload = ModifyBalancePayload {
        header,
        sequence: payload_sequence.to_be_bytes(),
        chain_id: chain_id.to_be_bytes(),
        token_chain: token_chain.to_be_bytes(),
        token_address,
        kind,
        amount: amount.0,
        reason,
    };
    let mut body = vaa_header(emitter_chain, emitter_address, sequence);
    body.extend_from_slice(bytemuck::bytes_of(&payload));
    body
}

pub fn upgrade_contract_body(
    emitter_chain: u16,
    emitter_address: [u8; 32],
    sequence: u64,
    header: GovernanceHeader,
    new_contract: [u8; 32],
) -> Vec<u8> {
    let payload = UpgradeContractPayload {
        header,
        new_contract,
    };
    let mut body = vaa_header(emitter_chain, emitter_address, sequence);
    body.extend_from_slice(bytemuck::bytes_of(&payload));
    body
}

/// Verify VAA Shim `post_signatures`. `signature_block` is `guardian_index ‖ signature`
/// entries, 66 bytes each; see [`crate::guardians::signature_block`]. `total_signatures`
/// sizes the account and equals the block's entry count when one call posts them all.
pub fn post_signatures_ix(
    payer: &Pubkey,
    guardian_signatures: &Pubkey,
    guardian_set_index: u32,
    total_signatures: u8,
    signature_block: &[u8],
) -> SvmInstruction {
    assert_eq!(
        signature_block.len() % GUARDIAN_SIGNATURE_LENGTH,
        0,
        "signature block is whole 66-byte entries"
    );
    let count = (signature_block.len() / GUARDIAN_SIGNATURE_LENGTH) as u32;
    let prefix = PostSignaturesIxData::new(guardian_set_index, total_signatures, count);
    let mut data = Vec::with_capacity(PostSignaturesIxData::LEN + signature_block.len());
    data.extend_from_slice(prefix.as_bytes());
    data.extend_from_slice(signature_block);
    SvmInstruction {
        program_id: shim_program_id(),
        accounts: vec![
            AccountMeta::new(*payer, true),
            AccountMeta::new(*guardian_signatures, true),
            AccountMeta::new_readonly(system_program_id(), false),
        ],
        data,
    }
}
