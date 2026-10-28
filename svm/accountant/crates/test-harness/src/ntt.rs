//! NTT observation, body and layout builders shared by the program tests and the Go codegen.

use std::mem::size_of;

use accountant_operational_core::hash::double_keccak256;
use accountant_operational_core::support::quorum::{observation_digests, ObservationDigests};
pub use global_accountant_definitions::instructions::ntt_global_accountant::Instruction as NttInstruction;
use global_accountant_definitions::{
    BelongsToHub, ManagerHead, NativeTokenTransfer, NttSubmitObservationsIxData, TransceiverHead,
    TransceiverHubKey, TransceiverHubLayout, TransceiverPeerKey, TransceiverPeerLayout, TxId,
    NATIVE_TOKEN_TRANSFER_PREFIX, NTT_SUBMIT_OBSERVATION_PREFIX, SIGNATURE_TX_ID_LEN,
    TRANSCEIVER_MESSAGE_PREFIX,
};

use crate::ix::{vaa_header, TX_ID};
use crate::wire;

pub fn hub_layout(
    chain: u16,
    address: [u8; 32],
    hub_chain: u16,
    hub: [u8; 32],
) -> TransceiverHubLayout {
    TransceiverHubLayout::new(
        TransceiverHubKey::new(chain, address),
        BelongsToHub(TransceiverHubKey::new(hub_chain, hub)),
    )
}

pub fn peer_layout(
    chain: u16,
    address: [u8; 32],
    dest_chain: u16,
    peer: [u8; 32],
) -> TransceiverPeerLayout {
    TransceiverPeerLayout::new(TransceiverPeerKey::new(chain, address, dest_chain), peer)
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

/// `TransceiverMessage` carrying a `NativeTokenTransfer` of `amount` at `decimals` to
/// `to_chain`, with empty additional and transceiver payloads.
pub fn transfer_payload(decimals: u8, amount: u64, to_chain: u16) -> Vec<u8> {
    let transfer = NativeTokenTransfer {
        prefix: NATIVE_TOKEN_TRANSFER_PREFIX,
        decimals,
        amount: amount.to_be_bytes(),
        source_token: [0x33; 32],
        to: [0x44; 32],
        to_chain: to_chain.to_be_bytes(),
    };
    let manager = ManagerHead {
        id: [0x55; 32],
        sender: [0x66; 32],
        payload_len: (size_of::<NativeTokenTransfer>() as u16).to_be_bytes(),
    };
    let head = TransceiverHead {
        prefix: TRANSCEIVER_MESSAGE_PREFIX,
        source_ntt_manager: [0x11; 32],
        recipient_ntt_manager: [0x22; 32],
        ntt_manager_payload_len: ((size_of::<ManagerHead>() + size_of::<NativeTokenTransfer>())
            as u16)
            .to_be_bytes(),
    };
    [
        bytemuck::bytes_of(&head),
        bytemuck::bytes_of(&manager),
        bytemuck::bytes_of(&transfer),
        &0u16.to_be_bytes(),
    ]
    .concat()
}

/// The fields of one NTT observation, as the guardian node signs them.
#[derive(Clone, Copy, Debug)]
pub struct Observation {
    pub chain: u16,
    pub emitter: [u8; 32],
    pub sequence: u64,
    pub sender: [u8; 32],
    pub recipient_chain: u16,
    pub trimmed_decimals: u8,
    pub trimmed_amount: u64,
    /// `double_keccak256` of the VAA body.
    pub digest: [u8; 32],
}

impl Observation {
    /// `sender` on `chain` published directly.
    pub fn direct(
        chain: u16,
        sender: [u8; 32],
        sequence: u64,
        recipient_chain: u16,
        trimmed_decimals: u8,
        trimmed_amount: u64,
    ) -> Self {
        let payload = transfer_payload(trimmed_decimals, trimmed_amount, recipient_chain);
        Self {
            chain,
            emitter: sender,
            sequence,
            sender,
            recipient_chain,
            trimmed_decimals,
            trimmed_amount,
            digest: double_keccak256(&direct_body(chain, sender, sequence, &payload)),
        }
    }

    /// `relayer` on `chain` published a `DeliveryInstruction` from `sender`.
    pub fn relayed(
        chain: u16,
        relayer: [u8; 32],
        sender: [u8; 32],
        sequence: u64,
        recipient_chain: u16,
        trimmed_decimals: u8,
        trimmed_amount: u64,
    ) -> Self {
        let payload = transfer_payload(trimmed_decimals, trimmed_amount, recipient_chain);
        Self {
            chain,
            emitter: relayer,
            sequence,
            sender,
            recipient_chain,
            trimmed_decimals,
            trimmed_amount,
            digest: double_keccak256(&relayed_body(chain, relayer, sequence, sender, &payload)),
        }
    }

    pub fn is_relayed(&self) -> bool {
        self.emitter != self.sender
    }

    /// The instruction struct a guardian sends for this observation.
    pub fn ix(
        &self,
        guardian_set_index: u32,
        guardian_index: u8,
        signature: [u8; 65],
        tx_id: TxId<'_>,
    ) -> NttSubmitObservationsIxData {
        let tx_id_bytes = tx_id.as_bytes();
        let mut tx_id_padded = [0u8; SIGNATURE_TX_ID_LEN];
        tx_id_padded[..tx_id_bytes.len()].copy_from_slice(tx_id_bytes);
        NttSubmitObservationsIxData {
            guardian_set_index: guardian_set_index.to_le_bytes(),
            guardian_index,
            signature,
            tx_id_len: u8::try_from(tx_id_bytes.len()).expect("tx id length fits u8"),
            tx_id: tx_id_padded,
            chain: self.chain.to_be_bytes(),
            emitter: self.emitter,
            sequence: self.sequence.to_be_bytes(),
            sender: self.sender,
            recipient_chain: self.recipient_chain.to_be_bytes(),
            trimmed_decimals: self.trimmed_decimals,
            trimmed_amount: self.trimmed_amount.to_be_bytes(),
            digest: self.digest,
        }
    }

    fn digests_with(&self, prefix: &[u8], tx_id: TxId<'_>) -> ObservationDigests {
        let ix = self.ix(0, 0, [0; 65], tx_id);
        observation_digests(prefix, tx_id, &ix.fields_and_digest())
    }

    pub fn content_digest(&self) -> [u8; 32] {
        self.digests_with(NTT_SUBMIT_OBSERVATION_PREFIX, TX_ID)
            .content
    }

    pub fn signing_digest(&self) -> [u8; 32] {
        self.signing_digest_with(NTT_SUBMIT_OBSERVATION_PREFIX, TX_ID)
    }

    pub fn signing_digest_with(&self, prefix: &[u8], tx_id: TxId<'_>) -> [u8; 32] {
        self.digests_with(prefix, tx_id).signing
    }
}
