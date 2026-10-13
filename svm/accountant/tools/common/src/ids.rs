//! Domain identifiers. Each unit has its own type, so a swapped argument fails
//! to compile. Derived `Ord` equals the numeric order of the inner value, and
//! for the struct keys the field order, as the tuple keys they replace.

use std::fmt;

/// Wormhole chain id.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ChainId(pub u16);

/// VAA sequence, or a governance payload sequence.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Sequence(pub u64);

/// 32-byte universal address: an emitter, a token, a transceiver or a peer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Address(pub [u8; 32]);

/// 32-byte VAA body digest.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Digest(pub [u8; 32]);

/// One Balance account: `(chain, token_chain, token_address)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BalanceKey {
    pub chain: ChainId,
    pub token_chain: ChainId,
    pub token_address: Address,
}

/// One emitter: `(chain, emitter)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct EmitterKey {
    pub chain: ChainId,
    pub emitter: Address,
}

/// One message: `(emitter chain, emitter, sequence)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MessageId {
    pub emitter: EmitterKey,
    pub sequence: Sequence,
}

/// One NoReplay bitmap PDA: an emitter and a bucket index (`sequence / 1024`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BucketId {
    pub emitter: EmitterKey,
    pub index: u64,
}

impl fmt::Display for ChainId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl fmt::Display for Sequence {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl fmt::Display for Address {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "0x{}", hex::encode(self.0))
    }
}

impl fmt::Display for Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "0x{}", hex::encode(self.0))
    }
}

impl fmt::Display for BalanceKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "chain={} token_chain={} token={}",
            self.chain, self.token_chain, self.token_address
        )
    }
}

impl fmt::Display for MessageId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "chain={} emitter={} sequence={}",
            self.emitter.chain, self.emitter.emitter, self.sequence
        )
    }
}
