//! Program-owned PDA keys. Each key holds its seed fields as the bytes the seeds need, so
//! derivation, address checks and creation all read one seed list per PDA kind.

use crate::constants::seeds::*;

/// Largest seed count of any key ([`PendingObservationsKey`]).
pub const MAX_SEEDS: usize = 6;

/// Seed slices of one PDA, without the bump.
#[derive(Clone, Copy)]
pub struct Seeds<'a> {
    items: [&'a [u8]; MAX_SEEDS],
    len: usize,
}

impl<'a> Seeds<'a> {
    pub fn new<const N: usize>(items: [&'a [u8]; N]) -> Self {
        const { assert!(N <= MAX_SEEDS) }
        let mut all: [&'a [u8]; MAX_SEEDS] = [&[]; MAX_SEEDS];
        all[..N].copy_from_slice(&items);
        Self { items: all, len: N }
    }

    pub fn as_slice(&self) -> &[&'a [u8]] {
        &self.items[..self.len]
    }
}

/// A PDA kind's canonical seeds.
pub trait PdaSeeds {
    fn seeds(&self) -> Seeds<'_>;
}

/// `(b"chain_registration", chain_be)`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ChainRegistrationKey {
    chain: [u8; 2],
}

impl ChainRegistrationKey {
    pub fn new(chain: u16) -> Self {
        Self {
            chain: chain.to_be_bytes(),
        }
    }
}

impl PdaSeeds for ChainRegistrationKey {
    fn seeds(&self) -> Seeds<'_> {
        Seeds::new([CHAIN_REGISTRATION_SEED_PREFIX, &self.chain])
    }
}

/// `(b"account", chain_be, token_chain_be, token_address)`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BalanceAccountKey {
    chain: [u8; 2],
    token_chain: [u8; 2],
    token_address: [u8; 32],
}

impl BalanceAccountKey {
    pub fn new(chain: u16, token_chain: u16, token_address: [u8; 32]) -> Self {
        Self {
            chain: chain.to_be_bytes(),
            token_chain: token_chain.to_be_bytes(),
            token_address,
        }
    }
}

impl PdaSeeds for BalanceAccountKey {
    fn seeds(&self) -> Seeds<'_> {
        Seeds::new([
            ACCOUNT_SEED_PREFIX,
            &self.chain,
            &self.token_chain,
            &self.token_address,
        ])
    }
}

/// `(b"register_chain", sequence_be)`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RegisterChainKey {
    sequence: [u8; 8],
}

impl RegisterChainKey {
    pub fn new(sequence: u64) -> Self {
        Self {
            sequence: sequence.to_be_bytes(),
        }
    }
}

impl PdaSeeds for RegisterChainKey {
    fn seeds(&self) -> Seeds<'_> {
        Seeds::new([REGISTER_CHAIN_SEED_PREFIX, &self.sequence])
    }
}

/// `(b"modify_balance", sequence_be)`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ModifyBalanceKey {
    sequence: [u8; 8],
}

impl ModifyBalanceKey {
    pub fn new(sequence: u64) -> Self {
        Self {
            sequence: sequence.to_be_bytes(),
        }
    }
}

impl PdaSeeds for ModifyBalanceKey {
    fn seeds(&self) -> Seeds<'_> {
        Seeds::new([MODIFY_BALANCE_SEED_PREFIX, &self.sequence])
    }
}

/// `(b"pending", chain_be, emitter, sequence_be, guardian_set_index_be, content_digest)`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PendingObservationsKey {
    chain: [u8; 2],
    emitter: [u8; 32],
    sequence: [u8; 8],
    guardian_set_index: [u8; 4],
    content_digest: [u8; 32],
}

impl PendingObservationsKey {
    pub fn new(
        chain: u16,
        emitter: [u8; 32],
        sequence: u64,
        guardian_set_index: u32,
        content_digest: [u8; 32],
    ) -> Self {
        Self {
            chain: chain.to_be_bytes(),
            emitter,
            sequence: sequence.to_be_bytes(),
            guardian_set_index: guardian_set_index.to_be_bytes(),
            content_digest,
        }
    }
}

impl PdaSeeds for PendingObservationsKey {
    fn seeds(&self) -> Seeds<'_> {
        Seeds::new([
            PENDING_OBSERVATIONS_SEED_PREFIX,
            &self.chain,
            &self.emitter,
            &self.sequence,
            &self.guardian_set_index,
            &self.content_digest,
        ])
    }
}

/// A transceiver `(chain, address)`; the `TransceiverHub` PDA key
/// `(b"transceiver_hub", chain_be, address)` and the value a hub entry points at.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TransceiverHubKey {
    chain: [u8; 2],
    pub address: [u8; 32],
}

impl TransceiverHubKey {
    pub fn new(chain: u16, address: [u8; 32]) -> Self {
        Self {
            chain: chain.to_be_bytes(),
            address,
        }
    }

    pub fn chain(&self) -> u16 {
        u16::from_be_bytes(self.chain)
    }
}

impl PdaSeeds for TransceiverHubKey {
    fn seeds(&self) -> Seeds<'_> {
        Seeds::new([TRANSCEIVER_HUB_SEED_PREFIX, &self.chain, &self.address])
    }
}

/// `(b"transceiver_peer", chain_be, address, dest_chain_be)`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TransceiverPeerKey {
    chain: [u8; 2],
    pub address: [u8; 32],
    dest_chain: [u8; 2],
}

impl TransceiverPeerKey {
    pub fn new(chain: u16, address: [u8; 32], dest_chain: u16) -> Self {
        Self {
            chain: chain.to_be_bytes(),
            address,
            dest_chain: dest_chain.to_be_bytes(),
        }
    }

    pub fn chain(&self) -> u16 {
        u16::from_be_bytes(self.chain)
    }

    pub fn dest_chain(&self) -> u16 {
        u16::from_be_bytes(self.dest_chain)
    }
}

impl PdaSeeds for TransceiverPeerKey {
    fn seeds(&self) -> Seeds<'_> {
        Seeds::new([
            TRANSCEIVER_PEER_SEED_PREFIX,
            &self.chain,
            &self.address,
            &self.dest_chain,
        ])
    }
}

#[cfg(test)]
mod tests {
    use std::vec;
    use std::vec::Vec;

    use super::*;

    /// One row: name, seeds from the key, hand-written seeds.
    type Case = (&'static str, Vec<Vec<u8>>, Vec<Vec<u8>>);

    fn owned(key: &impl PdaSeeds) -> Vec<Vec<u8>> {
        key.seeds().as_slice().iter().map(|s| s.to_vec()).collect()
    }

    /// Each key's seeds equal the hand-written lists the handlers used before the keys
    /// existed, so every PDA address stays where it is.
    #[test]
    fn seeds_match_hand_written_lists() {
        let emitter = [0xAA; 32];
        let digest = [0xBB; 32];
        let token = [0xCC; 32];
        let cases: [Case; 7] = [
            (
                "chain registration",
                owned(&ChainRegistrationKey::new(2)),
                vec![b"chain_registration".to_vec(), 2u16.to_be_bytes().to_vec()],
            ),
            (
                "balance",
                owned(&BalanceAccountKey::new(2, 1, token)),
                vec![
                    b"account".to_vec(),
                    2u16.to_be_bytes().to_vec(),
                    1u16.to_be_bytes().to_vec(),
                    token.to_vec(),
                ],
            ),
            (
                "register chain",
                owned(&RegisterChainKey::new(7)),
                vec![b"register_chain".to_vec(), 7u64.to_be_bytes().to_vec()],
            ),
            (
                "modify balance",
                owned(&ModifyBalanceKey::new(9)),
                vec![b"modify_balance".to_vec(), 9u64.to_be_bytes().to_vec()],
            ),
            (
                "pending",
                owned(&PendingObservationsKey::new(2, emitter, 5, 4, digest)),
                vec![
                    b"pending".to_vec(),
                    2u16.to_be_bytes().to_vec(),
                    emitter.to_vec(),
                    5u64.to_be_bytes().to_vec(),
                    4u32.to_be_bytes().to_vec(),
                    digest.to_vec(),
                ],
            ),
            (
                "transceiver hub",
                owned(&TransceiverHubKey::new(2, emitter)),
                vec![
                    b"transceiver_hub".to_vec(),
                    2u16.to_be_bytes().to_vec(),
                    emitter.to_vec(),
                ],
            ),
            (
                "transceiver peer",
                owned(&TransceiverPeerKey::new(2, emitter, 1)),
                vec![
                    b"transceiver_peer".to_vec(),
                    2u16.to_be_bytes().to_vec(),
                    emitter.to_vec(),
                    1u16.to_be_bytes().to_vec(),
                ],
            ),
        ];
        for (name, got, expected) in cases {
            assert_eq!(got, expected, "{name}");
        }
    }
}
