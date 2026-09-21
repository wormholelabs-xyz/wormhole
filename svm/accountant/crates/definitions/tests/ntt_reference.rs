//! NTT parsers checked against the reference encoders (`ntt-messages` at the rev wormchain
//! pins) and the mainnet vector corpus.

use std::collections::HashMap;

use global_accountant_definitions::*;
use wormhole_io::TypePrefixedPayload;

/// Constants equal `ntt-messages`; keccak prefixes also match the Solana hasher.
#[test]
fn prefixes_match_ntt_messages() {
    use ntt_messages::transceiver::Transceiver;
    use ntt_messages::transceivers::wormhole::WormholeTransceiver;

    assert_eq!(TRANSCEIVER_MESSAGE_PREFIX, WormholeTransceiver::PREFIX);
    assert_eq!(TRANSCEIVER_INFO_PREFIX, WormholeTransceiver::INFO_PREFIX);
    assert_eq!(
        TRANSCEIVER_PEER_INFO_PREFIX,
        WormholeTransceiver::PEER_INFO_PREFIX
    );
    // `NativeTokenTransfer::PREFIX` is private; read it off the encoding.
    let encoded = reference_native_transfer(8, 1, 1).to_vec_payload();
    assert_eq!(NATIVE_TOKEN_TRANSFER_PREFIX, encoded[..4]);
    assert_eq!(
        TRIMMED_DECIMALS,
        ntt_messages::trimmed_amount::TRIMMED_DECIMALS
    );

    let derived: [(&str, [u8; 4], &[u8]); 2] = [
        (
            "transceiver info",
            TRANSCEIVER_INFO_PREFIX,
            b"WormholeTransceiverInit",
        ),
        (
            "peer registration",
            TRANSCEIVER_PEER_INFO_PREFIX,
            b"WormholePeerRegistration",
        ),
    ];
    for (name, actual, seed) in derived {
        let hash = solana_program::keccak::hashv(&[seed]).to_bytes();
        assert_eq!(actual, hash[..4], "{name} keccak derivation");
    }
}

fn reference_native_transfer(
    decimals: u8,
    amount: u64,
    to_chain: u16,
) -> ntt_messages::ntt::NativeTokenTransfer {
    ntt_messages::ntt::NativeTokenTransfer {
        amount: ntt_messages::trimmed_amount::TrimmedAmount::new(amount, decimals),
        source_token: [0xEE; 32],
        to_chain: ntt_messages::chain_id::ChainId { id: to_chain },
        to: [0xFF; 32],
    }
}

/// `ntt-messages` encodings parse here, length prefixes included.
#[test]
fn parses_ntt_messages_encodings() {
    use ntt_messages::mode::Mode;
    use ntt_messages::ntt_manager::NttManagerMessage;
    use ntt_messages::transceiver::TransceiverMessage;
    use ntt_messages::transceivers::wormhole::{
        WormholeTransceiver, WormholeTransceiverInfo, WormholeTransceiverRegistration,
    };

    let transfer = TransceiverMessage::<WormholeTransceiver, _>::new(
        [0xAA; 32],
        [0xBB; 32],
        NttManagerMessage {
            id: [0xCC; 32],
            sender: [0xDD; 32],
            payload: reference_native_transfer(18, 1_000_000_000_000_000_000, 10),
        },
        std::vec![0x01, 0x02, 0x03],
    );
    assert_eq!(
        parse_ntt_transfer(&transfer.to_vec_payload()),
        Ok(NttTransfer {
            amount: Uint256::from_u128(100_000_000),
            recipient_chain: 10,
        })
    );

    for (mode, expected) in [
        (Mode::Locking, ManagerMode::Locking),
        (Mode::Burning, ManagerMode::Burning),
    ] {
        let info = WormholeTransceiverInfo {
            manager_address: [0x11; 32],
            manager_mode: mode,
            token_address: [0x22; 32],
            token_decimals: 8,
        }
        .to_vec_payload();
        let info = TransceiverInfo::from_payload(&info).expect("info");
        assert_eq!(info.mode, expected, "{mode}");
        assert_eq!(info.manager_address, [0x11; 32]);
    }

    let registration = WormholeTransceiverRegistration {
        chain_id: ntt_messages::chain_id::ChainId { id: 10 },
        transceiver_address: [0x77; 32],
    }
    .to_vec_payload();
    let view =
        TransceiverRegistrationPayload::from_payload(&registration).expect("registration view");
    assert_eq!(
        (view.dest_chain(), view.transceiver_address),
        (10, [0x77; 32])
    );
}

fn double_keccak256(body: &[u8]) -> [u8; 32] {
    let inner = solana_program::keccak::hashv(&[body]).to_bytes();
    solana_program::keccak::hashv(&[&inner]).to_bytes()
}

fn hex_bytes(s: &str) -> Vec<u8> {
    let s = s.strip_prefix("0x").unwrap_or(s);
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("hex byte"))
        .collect()
}

fn hex32(s: &str) -> [u8; 32] {
    let mut a = [0u8; 32];
    a.copy_from_slice(&hex_bytes(s));
    a
}

fn u16_field(v: &serde_json::Value, k: &str) -> u16 {
    v[k].as_u64().unwrap_or_else(|| panic!("field {k}")) as u16
}

/// Mainnet VAAs parse to what the wormchain NTT accountant committed.
#[test]
fn mainnet_vectors_parse_to_committed_accounting() {
    let corpus: serde_json::Value =
        serde_json::from_str(accountant_test_fixtures::NTT_TEST_VECTORS).expect("corpus");

    let mut hubs: HashMap<(u16, [u8; 32]), (u16, [u8; 32])> = HashMap::new();
    for h in corpus["hubs"].as_array().expect("hubs") {
        hubs.insert(
            (u16_field(h, "chain"), hex32(h["address"].as_str().unwrap())),
            (
                u16_field(h, "hub_chain"),
                hex32(h["hub_address"].as_str().unwrap()),
            ),
        );
    }

    let vectors = corpus["vectors"].as_array().expect("vectors");
    assert!(vectors.len() >= 28, "full corpus, got {}", vectors.len());

    let (mut relayer, mut direct) = (0usize, 0usize);
    for v in vectors {
        let chain = u16_field(v, "chain");
        let seq = v["sequence"].as_u64().expect("sequence");
        let label: String = std::format!("chain={chain} seq={seq}");

        let vaa = hex_bytes(v["vaa_hex"].as_str().expect("vaa_hex"));
        let num_sigs = vaa[5] as usize;
        let body = &vaa[6 + 66 * num_sigs..];
        assert_eq!(
            double_keccak256(body),
            hex32(v["expected_digest"].as_str().expect("expected_digest")),
            "[{label}] committed digest"
        );

        let payload = &body[51..];
        let emitter = hex32(v["emitter"].as_str().expect("emitter"));
        let (sender, ntt_payload): ([u8; 32], &[u8]) =
            if v["via_relayer"].as_bool().expect("via_relayer") {
                relayer += 1;
                let unwrap = parse_delivery_instruction(payload)
                    .unwrap_or_else(|e| panic!("[{label}] delivery: {e:?}"));
                (unwrap.sender, unwrap.inner_payload)
            } else {
                direct += 1;
                (emitter, payload)
            };

        let hub = hubs
            .get(&(chain, sender))
            .unwrap_or_else(|| panic!("[{label}] no hub for (chain, sender)"));
        assert_eq!(
            *hub,
            (
                u16_field(v, "expected_token_chain"),
                hex32(
                    v["expected_token_address"]
                        .as_str()
                        .expect("expected_token_address")
                ),
            ),
            "[{label}] hub token identity"
        );

        let transfer =
            parse_ntt_transfer(ntt_payload).unwrap_or_else(|e| panic!("[{label}] transfer: {e:?}"));
        assert_eq!(
            transfer.amount,
            Uint256::from_be_bytes(hex32(
                v["expected_amount"].as_str().expect("expected_amount")
            )),
            "[{label}] normalized amount"
        );
        assert_eq!(
            transfer.recipient_chain,
            u16_field(v, "expected_recipient_chain"),
            "[{label}] recipient chain"
        );
    }
    assert!(
        relayer > 0 && direct > 0,
        "{relayer} relayer, {direct} direct"
    );
}
