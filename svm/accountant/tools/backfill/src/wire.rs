//! Transaction wire limits and sizing, shared by the batcher and the instruction builder.

use solana_hash::Hash;
use solana_instruction::Instruction;
use solana_message::Message;
use solana_pubkey::Pubkey;
use solana_transaction::Transaction;

/// Solana's raw transaction wire limit. Every validator rejects a larger transaction.
pub const PACKET_DATA_SIZE: usize = 1232;

/// Base fee of a single-signature transaction. Deterministic on mainnet.
pub const TX_FEE_LAMPORTS: u64 = 5_000;

/// Serialized size of a one-signature legacy transaction carrying `ix`.
pub fn legacy_wire_size(ix: &Instruction, payer: &Pubkey) -> usize {
    let message =
        Message::new_with_blockhash(std::slice::from_ref(ix), Some(payer), &Hash::default());
    bincode::serialize(&Transaction::new_unsigned(message))
        .expect("serializing a well-formed Transaction cannot fail")
        .len()
}
