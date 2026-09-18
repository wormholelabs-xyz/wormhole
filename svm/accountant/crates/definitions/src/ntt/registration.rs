//! Wormhole transceiver hub and peer registration messages, as fixed-size views.

use bytemuck::{Pod, Zeroable};

use crate::error::GlobalAccountantError;

/// `WH_TRANSCEIVER_INIT_PREFIX`: hub registration message.
pub const TRANSCEIVER_INFO_PREFIX: [u8; 4] = keccak_prefix(b"WormholeTransceiverInit");
/// `WH_PEER_REGISTRATION_PREFIX`: peer registration message.
pub const TRANSCEIVER_PEER_INFO_PREFIX: [u8; 4] = keccak_prefix(b"WormholePeerRegistration");

/// Solidity `bytes4(keccak256(name))`.
const fn keccak_prefix(name: &[u8]) -> [u8; 4] {
    let hash = const_crypto::sha3::Keccak256::new().update(name).finalize();
    [hash[0], hash[1], hash[2], hash[3]]
}

/// `WormholeTransceiverInfo`: hub registration message, 70 bytes.
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq, Pod, Zeroable)]
pub struct TransceiverInfoPayload {
    pub prefix: [u8; 4],
    pub manager_address: [u8; 32],
    /// `0` Locking, `1` Burning.
    pub mode: u8,
    pub token_address: [u8; 32],
    pub token_decimals: u8,
}

impl TransceiverInfoPayload {
    pub const LEN: usize = core::mem::size_of::<Self>();
    pub const MODE_LOCKING: u8 = 0;
    pub const MODE_BURNING: u8 = 1;

    /// Exact length and `INFO_PREFIX`, else `MalformedNttMessage`.
    pub fn from_payload(payload: &[u8]) -> Result<&Self, GlobalAccountantError> {
        let view: &Self = bytemuck::try_from_bytes(payload)
            .map_err(|_| GlobalAccountantError::MalformedNttMessage)?;
        if view.prefix != TRANSCEIVER_INFO_PREFIX {
            return Err(GlobalAccountantError::MalformedNttMessage);
        }
        Ok(view)
    }

    /// Locking registers a hub; other modes are invalid.
    pub fn is_locking(&self) -> Result<bool, GlobalAccountantError> {
        match self.mode {
            Self::MODE_LOCKING => Ok(true),
            Self::MODE_BURNING => Ok(false),
            _ => Err(GlobalAccountantError::MalformedNttMessage),
        }
    }
}

/// `WormholeTransceiverRegistration`: peer registration message, 38 bytes.
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq, Pod, Zeroable)]
pub struct TransceiverRegistrationPayload {
    pub prefix: [u8; 4],
    pub chain: [u8; 2],
    pub transceiver_address: [u8; 32],
}

impl TransceiverRegistrationPayload {
    pub const LEN: usize = core::mem::size_of::<Self>();

    /// Exact length and `PEER_INFO_PREFIX`, else `MalformedNttMessage`.
    pub fn from_payload(payload: &[u8]) -> Result<&Self, GlobalAccountantError> {
        let view: &Self = bytemuck::try_from_bytes(payload)
            .map_err(|_| GlobalAccountantError::MalformedNttMessage)?;
        if view.prefix != TRANSCEIVER_PEER_INFO_PREFIX {
            return Err(GlobalAccountantError::MalformedNttMessage);
        }
        Ok(view)
    }

    pub fn dest_chain(&self) -> u16 {
        u16::from_be_bytes(self.chain)
    }
}

const _: () = {
    use core::mem::offset_of;
    assert!(TransceiverInfoPayload::LEN == 70);
    assert!(offset_of!(TransceiverInfoPayload, manager_address) == 4);
    assert!(offset_of!(TransceiverInfoPayload, mode) == 36);
    assert!(offset_of!(TransceiverInfoPayload, token_address) == 37);
    assert!(offset_of!(TransceiverInfoPayload, token_decimals) == 69);

    assert!(TransceiverRegistrationPayload::LEN == 38);
    assert!(offset_of!(TransceiverRegistrationPayload, chain) == 4);
    assert!(offset_of!(TransceiverRegistrationPayload, transceiver_address) == 6);
};

#[cfg(test)]
mod tests {
    use std::vec::Vec;

    use super::*;
    use GlobalAccountantError as E;

    /// One view row: name, wire bytes, expected result.
    type Case<R> = (&'static str, Vec<u8>, Result<R, E>);

    fn info(mode: u8) -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(&TRANSCEIVER_INFO_PREFIX);
        v.extend_from_slice(&[0x11; 32]);
        v.push(mode);
        v.extend_from_slice(&[0x22; 32]);
        v.push(8);
        v
    }

    fn registration() -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(&TRANSCEIVER_PEER_INFO_PREFIX);
        v.extend_from_slice(&10u16.to_be_bytes());
        v.extend_from_slice(&[0x77; 32]);
        v
    }

    #[test]
    fn registration_views_table() {
        let mut info_bad_prefix = info(0);
        info_bad_prefix[0] = 0;
        let mut info_long = info(0);
        info_long.push(0);
        let info_cases: [Case<Option<bool>>; 6] = [
            ("locking", info(0), Ok(Some(true))),
            ("burning", info(1), Ok(Some(false))),
            ("mode 2", info(2), Err(E::MalformedNttMessage)),
            ("bad prefix", info_bad_prefix, Err(E::MalformedNttMessage)),
            (
                "one byte short",
                info(0)[..69].to_vec(),
                Err(E::MalformedNttMessage),
            ),
            ("one byte long", info_long, Err(E::MalformedNttMessage)),
        ];
        for (name, payload, expected) in info_cases {
            let got = TransceiverInfoPayload::from_payload(&payload)
                .and_then(|view| view.is_locking().map(Some));
            assert_eq!(got, expected, "info {name}");
        }

        let mut reg_bad_prefix = registration();
        reg_bad_prefix[0] = 0;
        let mut reg_long = registration();
        reg_long.push(0);
        let reg_cases: [Case<(u16, [u8; 32])>; 4] = [
            ("well formed", registration(), Ok((10, [0x77; 32]))),
            ("bad prefix", reg_bad_prefix, Err(E::MalformedNttMessage)),
            (
                "one byte short",
                registration()[..37].to_vec(),
                Err(E::MalformedNttMessage),
            ),
            ("one byte long", reg_long, Err(E::MalformedNttMessage)),
        ];
        for (name, payload, expected) in reg_cases {
            let got = TransceiverRegistrationPayload::from_payload(&payload)
                .map(|view| (view.dest_chain(), view.transceiver_address));
            assert_eq!(got, expected, "registration {name}");
        }
    }
}
