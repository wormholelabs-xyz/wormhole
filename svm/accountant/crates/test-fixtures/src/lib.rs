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
pub struct Vaa<'a> {
    pub bytes: &'a [u8],
}

impl<'a> Vaa<'a> {
    /// Envelope: version (1) + guardian set index (4) + signature count (1) + 66 per signature.
    const ENVELOPE_FIXED: usize = 6;
    const SIGNATURE_LEN: usize = 66;

    pub fn guardian_set_index(&self) -> u32 {
        u32::from_be_bytes([self.bytes[1], self.bytes[2], self.bytes[3], self.bytes[4]])
    }

    pub fn signature_count(&self) -> u8 {
        self.bytes[5]
    }

    pub fn signatures(&self) -> &'a [u8] {
        let end = Self::ENVELOPE_FIXED + Self::SIGNATURE_LEN * self.signature_count() as usize;
        &self.bytes[Self::ENVELOPE_FIXED..end]
    }

    /// Body after the envelope.
    pub fn body(&self) -> &'a [u8] {
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
pub const MAINNET_TRANSFER_SEQ1395207: Vaa<'static> = Vaa {
    bytes: include_bytes!(data!("mainnet_solana_token_bridge_transfer_seq1395207.vaa")),
};

/// Solana Token Bridge message with non-transfer action 0x99, sequence 2211 (13 signatures).
pub const MAINNET_OTHER_SEQ2211: Vaa<'static> = Vaa {
    bytes: include_bytes!(data!("mainnet_solana_token_bridge_seq2211.vaa")),
};

/// 28 signed mainnet NTT VAAs (wormholescan) with what the wormchain NTT accountant committed
/// for each, from its state dump at height 18,669,029: the hub registry, and per transfer the
/// digest, normalized amount, hub-substituted token identity and recipient chain. Sampled two
/// transfers per `(chain, emitter)`; `via_relayer` is `emitter == chain's registered relayer`.
/// `{hubs: [{chain, address, hub_chain, hub_address}], vectors: [{chain, emitter, sequence,
/// via_relayer, expected_*, vaa_hex}]}`.
pub const NTT_TEST_VECTORS: &str = include_str!(data!("ntt_test_vectors.json"));

/// One `hubs` row of [`NTT_TEST_VECTORS`].
pub struct NttHub {
    pub chain: u16,
    pub address: [u8; 32],
    pub hub_chain: u16,
    pub hub_address: [u8; 32],
}

/// One `vectors` row of [`NTT_TEST_VECTORS`].
pub struct NttVector {
    pub chain: u16,
    pub emitter: [u8; 32],
    pub sequence: u64,
    pub via_relayer: bool,
    pub expected_digest: [u8; 32],
    /// Normalized amount, big-endian.
    pub expected_amount: [u8; 32],
    pub expected_token_chain: u16,
    pub expected_token_address: [u8; 32],
    pub expected_recipient_chain: u16,
    pub vaa: Vec<u8>,
}

impl NttVector {
    pub fn body(&self) -> &[u8] {
        Vaa { bytes: &self.vaa }.body()
    }

    pub fn label(&self) -> String {
        format!("chain={} seq={}", self.chain, self.sequence)
    }
}

pub struct NttCorpus {
    pub hubs: Vec<NttHub>,
    pub vectors: Vec<NttVector>,
}

impl NttCorpus {
    pub fn load() -> Self {
        let corpus: serde_json::Value =
            serde_json::from_str(NTT_TEST_VECTORS).expect("ntt_test_vectors.json");
        let hubs = corpus["hubs"]
            .as_array()
            .expect("hubs")
            .iter()
            .map(|h| NttHub {
                chain: u16_field(h, "chain"),
                address: hex32_field(h, "address"),
                hub_chain: u16_field(h, "hub_chain"),
                hub_address: hex32_field(h, "hub_address"),
            })
            .collect();
        let vectors = corpus["vectors"]
            .as_array()
            .expect("vectors")
            .iter()
            .map(|v| NttVector {
                chain: u16_field(v, "chain"),
                emitter: hex32_field(v, "emitter"),
                sequence: v["sequence"].as_u64().expect("sequence"),
                via_relayer: v["via_relayer"].as_bool().expect("via_relayer"),
                expected_digest: hex32_field(v, "expected_digest"),
                expected_amount: hex32_field(v, "expected_amount"),
                expected_token_chain: u16_field(v, "expected_token_chain"),
                expected_token_address: hex32_field(v, "expected_token_address"),
                expected_recipient_chain: u16_field(v, "expected_recipient_chain"),
                vaa: hex_bytes(v["vaa_hex"].as_str().expect("vaa_hex")),
            })
            .collect();
        Self { hubs, vectors }
    }

    /// Hub wormchain recorded for the transceiver `address` on `chain`.
    pub fn hub_for(&self, chain: u16, address: [u8; 32]) -> Option<(u16, [u8; 32])> {
        self.hubs
            .iter()
            .find(|h| h.chain == chain && h.address == address)
            .map(|h| (h.hub_chain, h.hub_address))
    }
}

fn hex_bytes(s: &str) -> Vec<u8> {
    let s = s.strip_prefix("0x").unwrap_or(s);
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("hex"))
        .collect()
}

fn hex32_field(v: &serde_json::Value, k: &str) -> [u8; 32] {
    hex_bytes(v[k].as_str().unwrap_or_else(|| panic!("field {k}")))
        .try_into()
        .unwrap_or_else(|_| panic!("field {k}: 32 bytes"))
}

fn u16_field(v: &serde_json::Value, k: &str) -> u16 {
    u16::try_from(v[k].as_u64().unwrap_or_else(|| panic!("field {k}")))
        .unwrap_or_else(|_| panic!("field {k}: u16"))
}

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
    fn ntt_corpus_loads_every_row() {
        let corpus = NttCorpus::load();
        assert_eq!(corpus.vectors.len(), 28);
        assert!(!corpus.hubs.is_empty());
        for v in &corpus.vectors {
            assert_eq!(v.vaa[0], 1, "{} VAA version", v.label());
            assert!(v.body().len() > 51, "{} body carries a payload", v.label());
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
