//! Commit-log entry emitted on quorum in `submit_observations` and on every successful
//! `submit_vaas`. Off-chain indexers filter on the tag prefix.

use bytemuck::{Pod, Zeroable};

/// 8-byte tag on every commit log entry.
pub const ACCOUNTANT_DIGEST_LOG_TAG: [u8; 8] = *b"ACCDGST\0";

/// Commit log entry (86 bytes). `guardian_set_index` is the quorum set on the observations
/// path; `submit_vaas` writes `0`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq, Pod, Zeroable)]
pub struct AccountantDigestLog {
    pub tag: [u8; 8],
    /// Big-endian.
    pub chain: [u8; 2],
    pub emitter: [u8; 32],
    /// Big-endian.
    pub sequence: [u8; 8],
    pub digest: [u8; 32],
    /// Little-endian.
    pub guardian_set_index: [u8; 4],
}

const _: () = {
    use core::mem::offset_of;
    assert!(AccountantDigestLog::LEN == 86);
    assert!(offset_of!(AccountantDigestLog, chain) == 8);
    assert!(offset_of!(AccountantDigestLog, emitter) == 10);
    assert!(offset_of!(AccountantDigestLog, sequence) == 42);
    assert!(offset_of!(AccountantDigestLog, digest) == 50);
    assert!(offset_of!(AccountantDigestLog, guardian_set_index) == 82);
};

impl AccountantDigestLog {
    pub const LEN: usize = core::mem::size_of::<Self>();

    pub fn new(
        chain: u16,
        emitter: [u8; 32],
        sequence: u64,
        digest: [u8; 32],
        guardian_set_index: u32,
    ) -> Self {
        Self {
            tag: ACCOUNTANT_DIGEST_LOG_TAG,
            chain: chain.to_be_bytes(),
            emitter,
            sequence: sequence.to_be_bytes(),
            digest,
            guardian_set_index: guardian_set_index.to_le_bytes(),
        }
    }

    pub fn as_bytes(&self) -> &[u8] {
        bytemuck::bytes_of(self)
    }

    /// Exact-length view; `None` on wrong length or tag.
    pub fn from_bytes(bytes: &[u8]) -> Option<&Self> {
        let entry: &Self = bytemuck::try_from_bytes(bytes).ok()?;
        (entry.tag == ACCOUNTANT_DIGEST_LOG_TAG).then_some(entry)
    }

    pub fn chain(&self) -> u16 {
        u16::from_be_bytes(self.chain)
    }

    pub fn sequence(&self) -> u64 {
        u64::from_be_bytes(self.sequence)
    }

    pub fn guardian_set_index(&self) -> u32 {
        u32::from_le_bytes(self.guardian_set_index)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_bytes() {
        let entry = AccountantDigestLog::new(2, [0xAA; 32], 7, [0xBB; 32], 3);
        let bytes = entry.as_bytes();
        assert_eq!(bytes.len(), AccountantDigestLog::LEN);
        assert_eq!(&bytes[..8], b"ACCDGST\0");
        assert_eq!(bytes[8..10], [0, 2]);
        assert_eq!(bytes[42..50], 7u64.to_be_bytes());
        assert_eq!(bytes[82..86], [3, 0, 0, 0]);

        let view = AccountantDigestLog::from_bytes(bytes).expect("view");
        assert_eq!(view.chain(), 2);
        assert_eq!(view.sequence(), 7);
        assert_eq!(view.guardian_set_index(), 3);
        assert_eq!(view, &entry);
    }

    #[test]
    fn from_bytes_rejects_wrong_length_and_tag() {
        let entry = AccountantDigestLog::new(2, [0; 32], 7, [0; 32], 0);
        let mut bytes = entry.as_bytes().to_vec();
        assert!(AccountantDigestLog::from_bytes(&bytes[..85]).is_none());
        bytes.push(0);
        assert!(AccountantDigestLog::from_bytes(&bytes).is_none());
        bytes.pop();
        bytes[0] ^= 1;
        assert!(AccountantDigestLog::from_bytes(&bytes).is_none());
    }
}
