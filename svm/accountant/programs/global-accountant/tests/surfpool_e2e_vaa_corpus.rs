//! In-process parser sanity check for the VAA fixtures, so format regressions
//! surface during `cargo test` before any surfpool layer boots.

mod common;
use common::{load_vaa_fixture, ParsedVaa};

/// Both fixtures parse, and the seq 2211 digest matches the pinned value used
/// by surfpool_e2e_mainnet_fork.
#[test]
fn corpus_fixtures_parse_and_digest_consistently() {
    const SEQ_2211_EXPECTED_DIGEST: [u8; 32] = [
        0xe4, 0xca, 0xc2, 0x84, 0x65, 0x6a, 0xc7, 0x4a, 0xd4, 0xef, 0x1b, 0x0e, 0xc7, 0xc2, 0xbe,
        0x76, 0x28, 0x97, 0x05, 0x45, 0x80, 0x71, 0xc7, 0xdd, 0xbe, 0xf8, 0x05, 0x49, 0x9a, 0x05,
        0x41, 0x16,
    ];
    let baseline = load_vaa_fixture("mainnet_solana_token_bridge_seq2211.vaa");
    assert_eq!(
        baseline.digest, SEQ_2211_EXPECTED_DIGEST,
        "seq 2211 digest pinned"
    );
    assert_eq!(baseline.guardian_set_index, 6);
    assert_eq!(baseline.emitter_chain, 1);
    assert_eq!(baseline.sequence, 2211);
    assert_eq!(baseline.num_signatures, 13);

    // Token Bridge transfer fixture (source for surfpool_e2e_submit_vaas). Pin
    // the digest and routing tuple so a corrupted or swapped fixture is caught
    // here, not only at the surfpool layer.
    const SEQ_1395207_EXPECTED_DIGEST: [u8; 32] = [
        0x89, 0xc4, 0x1f, 0x5a, 0xc9, 0xc3, 0x5b, 0xa9, 0xd1, 0x5b, 0xf3, 0x58, 0xb9, 0x31, 0xd6,
        0xf7, 0x54, 0xa7, 0x73, 0x49, 0x02, 0x2d, 0x0a, 0x64, 0xf1, 0x39, 0x62, 0x07, 0x8f, 0x63,
        0x2a, 0x39,
    ];
    let transfer = load_vaa_fixture("mainnet_solana_token_bridge_transfer_seq1395207.vaa");
    assert_eq!(transfer.guardian_set_index, 6);
    assert!(transfer.num_signatures >= 13);
    assert_eq!(
        transfer.signatures_slice().len(),
        (transfer.num_signatures as usize) * ParsedVaa::GUARDIAN_SIGNATURE_LENGTH,
    );
    assert_eq!(
        transfer.digest, SEQ_1395207_EXPECTED_DIGEST,
        "seq 1395207 transfer digest pinned"
    );
    assert_eq!(transfer.emitter_chain, 1);
    assert_eq!(transfer.sequence, 1_395_207);
}
