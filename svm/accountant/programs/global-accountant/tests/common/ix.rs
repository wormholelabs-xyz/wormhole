use global_accountant_definitions::{
    ClosePendingIxData, GovernanceHeader, Instruction, ModifyBalanceIxData, ModifyBalancePayload,
    RegisterChainIxData, RegisterChainPayload, SubmitObservationsIxData, SubmitVaasIxData,
    TokenBridgeTransfer, Uint256, UpgradeContractIxData, UpgradeContractPayload, VaaBodyHeader,
    SUBMIT_OBSERVATION_PREFIX,
};

pub use accountant_operational_core::hash::double_keccak256;

pub const TX_HASH: [u8; 32] = [0xA9u8; 32];
pub const RECIPIENT: [u8; 32] = [0xAB; 32];

pub fn vaa_header(emitter_chain: u16, emitter_address: [u8; 32], sequence: u64) -> Vec<u8> {
    let header = VaaBodyHeader::new(0, 0, emitter_chain, emitter_address, sequence, 0);
    bytemuck::bytes_of(&header).to_vec()
}

pub fn attest_body(emitter_chain: u16, emitter_address: [u8; 32], sequence: u64) -> Vec<u8> {
    let mut body = vaa_header(emitter_chain, emitter_address, sequence);
    body.push(0x02);
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
        0x01,
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

pub fn governance_header(module: [u8; 32], action: u8, target_chain: u16) -> GovernanceHeader {
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

pub fn signing_digest(body: &[u8]) -> [u8; 32] {
    accountant_operational_core::hash::observation_signing_digest(
        SUBMIT_OBSERVATION_PREFIX,
        &TX_HASH,
        body,
    )
}

fn framed(discriminator: Instruction, prefix: &[u8], body: &[u8]) -> Vec<u8> {
    let mut data = Vec::with_capacity(1 + prefix.len() + body.len());
    data.push(discriminator as u8);
    data.extend_from_slice(prefix);
    data.extend_from_slice(body);
    data
}

pub fn submit_observations_ix_data(
    guardian_set_index: u32,
    guardian_index: u8,
    signature: [u8; 65],
    body: &[u8],
) -> Vec<u8> {
    let prefix = SubmitObservationsIxData {
        guardian_set_index: guardian_set_index.to_le_bytes(),
        guardian_index,
        signature,
        tx_hash: TX_HASH,
        body_len: (body.len() as u16).to_le_bytes(),
    };
    framed(
        Instruction::SubmitObservations,
        bytemuck::bytes_of(&prefix),
        body,
    )
}

pub fn submit_vaas_ix_data(guardian_set_bump: u8, body: &[u8]) -> Vec<u8> {
    submit_vaas_ix_data_with_len(guardian_set_bump, body.len() as u16, body)
}

pub fn submit_vaas_ix_data_with_len(guardian_set_bump: u8, body_len: u16, body: &[u8]) -> Vec<u8> {
    let prefix = SubmitVaasIxData {
        guardian_set_bump,
        body_len: body_len.to_le_bytes(),
    };
    framed(Instruction::SubmitVaas, bytemuck::bytes_of(&prefix), body)
}

pub fn register_chain_ix_data(guardian_set_bump: u8, body: &[u8]) -> Vec<u8> {
    let prefix = RegisterChainIxData {
        guardian_set_bump,
        body_len: (body.len() as u16).to_le_bytes(),
    };
    framed(
        Instruction::RegisterChain,
        bytemuck::bytes_of(&prefix),
        body,
    )
}

pub fn modify_balance_ix_data(guardian_set_bump: u8, body: &[u8]) -> Vec<u8> {
    let prefix = ModifyBalanceIxData {
        guardian_set_bump,
        body_len: (body.len() as u16).to_le_bytes(),
    };
    framed(
        Instruction::ModifyBalance,
        bytemuck::bytes_of(&prefix),
        body,
    )
}

pub fn upgrade_contract_ix_data(guardian_set_bump: u8, body: &[u8]) -> Vec<u8> {
    let prefix = UpgradeContractIxData {
        guardian_set_bump,
        body_len: (body.len() as u16).to_le_bytes(),
    };
    framed(
        Instruction::UpgradeContract,
        bytemuck::bytes_of(&prefix),
        body,
    )
}

pub fn close_pending_ix_data(emitter: [u8; 32], sequence: u64) -> Vec<u8> {
    let data = ClosePendingIxData {
        emitter,
        sequence: sequence.to_be_bytes(),
    };
    framed(Instruction::ClosePending, bytemuck::bytes_of(&data), &[])
}
