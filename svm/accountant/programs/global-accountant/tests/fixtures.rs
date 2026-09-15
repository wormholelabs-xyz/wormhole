use accountant_operational_core::hash::double_keccak256;
use accountant_test_fixtures::{Vaa, MAINNET_OTHER_SEQ2211, MAINNET_TRANSFER_SEQ1395207};
use global_accountant_definitions::VaaBodyHeader;

mod common;
use common::GUARDIAN_SIGNATURE_LENGTH;

#[test]
fn fixtures_parse_with_pinned_digests() {
    let cases: [(&str, &Vaa, u64, [u8; 32]); 2] = [
        (
            "seq 2211",
            &MAINNET_OTHER_SEQ2211,
            2211,
            [
                0xe4, 0xca, 0xc2, 0x84, 0x65, 0x6a, 0xc7, 0x4a, 0xd4, 0xef, 0x1b, 0x0e, 0xc7, 0xc2,
                0xbe, 0x76, 0x28, 0x97, 0x05, 0x45, 0x80, 0x71, 0xc7, 0xdd, 0xbe, 0xf8, 0x05, 0x49,
                0x9a, 0x05, 0x41, 0x16,
            ],
        ),
        (
            "seq 1395207",
            &MAINNET_TRANSFER_SEQ1395207,
            1_395_207,
            [
                0x89, 0xc4, 0x1f, 0x5a, 0xc9, 0xc3, 0x5b, 0xa9, 0xd1, 0x5b, 0xf3, 0x58, 0xb9, 0x31,
                0xd6, 0xf7, 0x54, 0xa7, 0x73, 0x49, 0x02, 0x2d, 0x0a, 0x64, 0xf1, 0x39, 0x62, 0x07,
                0x8f, 0x63, 0x2a, 0x39,
            ],
        ),
    ];
    for (label, vaa, sequence, digest) in cases {
        let (header, _) = VaaBodyHeader::split(vaa.body()).expect(label);
        assert_eq!(vaa.guardian_set_index(), 6, "{label} guardian set");
        assert_eq!(vaa.signature_count(), 13, "{label} signatures");
        assert_eq!(
            vaa.signatures().len(),
            13 * GUARDIAN_SIGNATURE_LENGTH,
            "{label} signature block"
        );
        assert_eq!(header.emitter_chain(), 1, "{label} chain");
        assert_eq!(header.sequence(), sequence, "{label} sequence");
        assert_eq!(double_keccak256(vaa.body()), digest, "{label} digest");
    }
}
