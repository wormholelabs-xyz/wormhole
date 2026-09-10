//! Test artifacts, embedded at compile time. The only place fixture paths appear.
//!
//! ## Regenerate a program fixture
//!
//! 1. Build the sibling repo: `just build` in `solana-noreplay`, `make build` in
//!    `wormhole-core-shims`.
//! 2. Copy the `.so` over `data/<name>.so`.
//! 3. `shasum -a 256 data/<name>.so` and update the `sha256` field.

use std::path::Path;

/// A signed VAA, envelope included.
pub struct Vaa {
    pub bytes: &'static [u8],
}

impl Vaa {
    /// Envelope: version (1) + guardian set index (4) + signature count (1) + 66 per signature.
    const ENVELOPE_FIXED: usize = 6;
    const SIGNATURE_LEN: usize = 66;

    pub fn guardian_set_index(&self) -> u32 {
        u32::from_be_bytes([self.bytes[1], self.bytes[2], self.bytes[3], self.bytes[4]])
    }

    pub fn signature_count(&self) -> u8 {
        self.bytes[5]
    }

    pub fn signatures(&self) -> &'static [u8] {
        let end = Self::ENVELOPE_FIXED + Self::SIGNATURE_LEN * self.signature_count() as usize;
        &self.bytes[Self::ENVELOPE_FIXED..end]
    }

    /// Body after the envelope.
    pub fn body(&self) -> &'static [u8] {
        let n_sigs = self.signature_count() as usize;
        &self.bytes[Self::ENVELOPE_FIXED + Self::SIGNATURE_LEN * n_sigs..]
    }
}

/// A sibling program's SBF binary with its pinned digest.
pub struct Program {
    pub bytes: &'static [u8],
    pub sha256: [u8; 32],
    path: &'static str,
}

impl Program {
    /// On-disk location, for harnesses that load by path (surfpool).
    pub fn path(&self) -> &'static Path {
        Path::new(self.path)
    }
}

macro_rules! data {
    ($name:literal) => {
        concat!(env!("CARGO_MANIFEST_DIR"), "/data/", $name)
    };
}

/// Solana Token Bridge transfer, Solana -> Ethereum, sequence 1395207 (13 signatures).
pub const MAINNET_TRANSFER_SEQ1395207: Vaa = Vaa {
    bytes: include_bytes!(data!("mainnet_solana_token_bridge_transfer_seq1395207.vaa")),
};

/// Solana Token Bridge message with non-transfer action 0x99, sequence 2211 (13 signatures).
pub const MAINNET_OTHER_SEQ2211: Vaa = Vaa {
    bytes: include_bytes!(data!("mainnet_solana_token_bridge_seq2211.vaa")),
};

pub const NOREPLAY_SO: Program = Program {
    bytes: include_bytes!(data!("solana_noreplay.so")),
    sha256: [
        0x33, 0xbe, 0x38, 0x6b, 0xac, 0xf5, 0x6b, 0x98, 0x98, 0xfb, 0xa7, 0x5b, 0x10, 0x48, 0xcb,
        0xe1, 0x17, 0x90, 0xf2, 0x42, 0xe9, 0x75, 0x07, 0xb4, 0x52, 0xeb, 0x7f, 0x08, 0xd0, 0x86,
        0x9a, 0xc7,
    ],
    path: data!("solana_noreplay.so"),
};

pub const VERIFY_VAA_SHIM_SO: Program = Program {
    bytes: include_bytes!(data!("wormhole_verify_vaa_shim.so")),
    sha256: [
        0xba, 0xc0, 0xee, 0x4b, 0xb4, 0xb1, 0x2b, 0xd4, 0xaf, 0x9c, 0xa9, 0xe4, 0x5a, 0xc6, 0x7e,
        0x4b, 0x77, 0x96, 0xc9, 0xf6, 0x04, 0x68, 0xa4, 0x4d, 0xa0, 0x3d, 0x16, 0x9c, 0x42, 0xaf,
        0xaa, 0x40,
    ],
    path: data!("wormhole_verify_vaa_shim.so"),
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vaa_fixtures_carry_thirteen_signatures_and_a_body() {
        for vaa in [&MAINNET_TRANSFER_SEQ1395207, &MAINNET_OTHER_SEQ2211] {
            assert_eq!(vaa.bytes[0], 1, "VAA version");
            assert_eq!(vaa.bytes[5], 13, "signature count");
            assert!(vaa.body().len() > 51, "body carries a payload");
        }
    }

    #[test]
    fn program_paths_exist() {
        for program in [&NOREPLAY_SO, &VERIFY_VAA_SHIM_SO] {
            assert!(program.path().is_file(), "{}", program.path().display());
            assert_eq!(std::fs::read(program.path()).unwrap(), program.bytes);
        }
    }
}
