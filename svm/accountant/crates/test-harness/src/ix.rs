//! VAA body builders for governance payloads, and instructions of sibling programs.

use global_accountant_definitions::{
    GovernanceHeader, GovernanceModule, ModifyBalancePayload, PostSignaturesIxData,
    RegisterChainPayload, SetComputeUnitLimitData, Uint256, UpgradeContractPayload, VaaBodyHeader,
};
use solana_instruction::{AccountMeta, Instruction as SvmInstruction};
use solana_pubkey::Pubkey;

use crate::guardians::GUARDIAN_SIGNATURE_LENGTH;
use crate::ids::{compute_budget_program_id, shim_program_id, system_program_id};

pub use accountant_operational_core::hash::double_keccak256;

pub const TX_HASH: [u8; 32] = [0xA9u8; 32];

pub fn vaa_header(emitter_chain: u16, emitter_address: [u8; 32], sequence: u64) -> Vec<u8> {
    let header = VaaBodyHeader::new(0, 0, emitter_chain, emitter_address, sequence, 0);
    bytemuck::bytes_of(&header).to_vec()
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

/// Compute Budget `SetComputeUnitLimit`.
pub fn set_compute_unit_limit_ix(units: u32) -> SvmInstruction {
    SvmInstruction {
        program_id: compute_budget_program_id(),
        accounts: vec![],
        data: SetComputeUnitLimitData::new(units).as_bytes().to_vec(),
    }
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
