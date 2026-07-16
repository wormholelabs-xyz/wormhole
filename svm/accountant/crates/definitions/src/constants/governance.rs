//! Wormhole governance identifiers: emitter, chain IDs, module and action bytes.

/// Wormhole governance emitter — `chain = 1 (Solana)`, `address = [0; 31] ||
/// 0x04`. `register_chain` only accepts governance VAAs signed by this emitter.
pub const GOVERNANCE_EMITTER: [u8; 32] = [
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x04,
];

/// Wormhole chain ID for Solana, also stamped on the governance emitter pair.
/// `register_chain` governance VAAs must target either chain `0x0000` (Any) or
/// this; `modify_balance` VAAs must target this.
pub const SOLANA_CHAIN_ID: u16 = 1;

/// Token Bridge governance module — first 32 bytes of a Token Bridge
/// governance payload. "TokenBridge" right-aligned in 32 bytes.
pub const TOKEN_BRIDGE_GOVERNANCE_MODULE: [u8; 32] = [
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, b'T', b'o', b'k', b'e', b'n', b'B', b'r', b'i', b'd', b'g', b'e',
];

/// Token Bridge governance `RegisterChain` action byte.
pub const REGISTER_CHAIN_ACTION: u8 = 0x01;

/// Accountant governance module — first 32 bytes of a `ModifyBalance` payload.
/// "GlobalAccountant" right-aligned in 32 bytes. The action byte `0x01`
/// overlaps RegisterChain, so the module is what disambiguates the flows.
pub const ACCOUNTANT_GOVERNANCE_MODULE: [u8; 32] = [
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    b'G', b'l', b'o', b'b', b'a', b'l', b'A', b'c', b'c', b'o', b'u', b'n', b't', b'a', b'n', b't',
];

/// Accountant governance `ModifyBalance` action byte.
pub const MODIFY_BALANCE_ACTION: u8 = 0x01;
