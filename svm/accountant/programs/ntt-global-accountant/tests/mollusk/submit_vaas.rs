//! `submit_vaas`: a signed NTT transfer moves balances keyed on the sender's hub once the
//! sender and its peer are cross-registered.

use std::collections::HashMap;

use global_accountant_definitions::{parse_delivery_instruction, GlobalAccountantError, Uint256};
use solana_pubkey::Pubkey;

use crate::common::*;

const SOLANA_HUB: (u16, [u8; 32]) = (SOLANA, HUB);
/// 1.5 tokens at 6 decimals; the accountant books it at 8.
const DECIMALS: u8 = 6;
const AMOUNT: u64 = 1_500_000;
const BOOKED: u128 = 150_000_000;

/// The Solana hub sends to its Ethereum spoke: native side locks, wrapped side mints.
fn hub_to_spoke(sequence: u64) -> VaaScenario {
    VaaScenario::direct(
        sequence,
        SOLANA,
        HUB,
        ETHEREUM,
        SPOKE,
        SOLANA_HUB,
        &transfer_payload(DECIMALS, AMOUNT, ETHEREUM),
    )
}

fn hub_to_spoke_accounts() -> VaaAccounts {
    VaaAccounts::registered(SOLANA, HUB, ETHEREUM, SPOKE, SOLANA_HUB)
}

#[test]
fn hub_transfer_locks_native_and_mints_wrapped() {
    let mollusk = mollusk();
    let transfer = hub_to_spoke(6);
    let result = transfer.submit(&mollusk, hub_to_spoke_accounts());
    assert_success(&result, "hub to spoke");
    let after = &result.resulting_accounts;
    assert_bucket_marked(
        find_account(after, &transfer.noreplay_bucket),
        transfer.sequence,
    );
    assert_balance(after, &transfer.source_balance, Uint256::from_u128(BOOKED));
    assert_balance(after, &transfer.dest_balance, Uint256::from_u128(BOOKED));
}

#[test]
fn relayed_spoke_transfer_burns_wrapped_and_unlocks_native() {
    let mollusk = mollusk();
    let transfer = VaaScenario::relayed(
        7,
        ETHEREUM,
        RELAYER,
        SPOKE,
        SOLANA,
        HUB,
        SOLANA_HUB,
        &transfer_payload(DECIMALS, AMOUNT, SOLANA),
    );
    let accounts = VaaAccounts {
        relayer_registration: chain_registration_account_for(&program_id(), ETHEREUM, RELAYER),
        source_balance: balance_account(ETHEREUM, SOLANA, HUB, Uint256::from_u128(BOOKED)),
        dest_balance: balance_account(SOLANA, SOLANA, HUB, Uint256::from_u128(BOOKED)),
        ..VaaAccounts::registered(ETHEREUM, SPOKE, SOLANA, HUB, SOLANA_HUB)
    };
    let result = transfer.submit(&mollusk, accounts);
    assert_success(&result, "spoke to hub via relayer");
    let after = &result.resulting_accounts;
    assert_bucket_marked(
        find_account(after, &transfer.noreplay_bucket),
        transfer.sequence,
    );
    assert_balance(after, &transfer.source_balance, Uint256::ZERO);
    assert_balance(after, &transfer.dest_balance, Uint256::ZERO);
}

#[test]
fn rejects() {
    let mollusk = mollusk();

    let mut spoofed_hub = hub_to_spoke(30);
    spoofed_hub.hub_pda = Pubkey::new_unique();
    let mut spoofed_source = hub_to_spoke(31);
    spoofed_source.source_balance = Pubkey::new_unique();
    let mut truncated = transfer_payload(DECIMALS, AMOUNT, ETHEREUM);
    truncated.truncate(100);
    let oversized = [
        transfer_payload(DECIMALS, AMOUNT, ETHEREUM),
        vec![0u8; 2000],
    ]
    .concat();
    let no_envelope = VaaScenario::build(
        36,
        ETHEREUM,
        RELAYER,
        SPOKE,
        SOLANA,
        HUB,
        SOLANA_HUB,
        direct_body(
            ETHEREUM,
            RELAYER,
            36,
            &transfer_payload(DECIMALS, AMOUNT, SOLANA),
        ),
    );
    let relayed_accounts = || VaaAccounts {
        relayer_registration: chain_registration_account_for(&program_id(), ETHEREUM, RELAYER),
        source_balance: balance_account(ETHEREUM, SOLANA, HUB, Uint256::from_u128(BOOKED)),
        ..VaaAccounts::registered(ETHEREUM, SPOKE, SOLANA, HUB, SOLANA_HUB)
    };

    type Row = (
        &'static str,
        VaaScenario,
        VaaAccounts,
        Option<u16>,
        GlobalAccountantError,
    );
    let cases: [Row; 13] = [
        (
            "pre-marked noreplay",
            hub_to_spoke(20),
            VaaAccounts {
                bucket: noreplay_bucket_marked(20),
                ..hub_to_spoke_accounts()
            },
            None,
            GlobalAccountantError::AlreadyAccounted,
        ),
        (
            "sender has no hub",
            hub_to_spoke(21),
            VaaAccounts {
                hub: uninitialised_pda_account(),
                ..hub_to_spoke_accounts()
            },
            None,
            GlobalAccountantError::MissingTransceiverHub,
        ),
        (
            "no source peer for the recipient chain",
            hub_to_spoke(22),
            VaaAccounts {
                peer_src: uninitialised_pda_account(),
                ..hub_to_spoke_accounts()
            },
            None,
            GlobalAccountantError::MissingSourcePeer,
        ),
        (
            "peer has not registered the sender",
            hub_to_spoke(23),
            VaaAccounts {
                peer_dst: uninitialised_pda_account(),
                ..hub_to_spoke_accounts()
            },
            None,
            GlobalAccountantError::MissingDestinationPeer,
        ),
        (
            "peer points back at another transceiver",
            hub_to_spoke(24),
            VaaAccounts {
                peer_dst: peer_account(&peer_layout(ETHEREUM, SPOKE, SOLANA, OTHER)),
                ..hub_to_spoke_accounts()
            },
            None,
            GlobalAccountantError::PeersNotCrossRegistered,
        ),
        (
            "wrapped source underflow",
            VaaScenario::direct(
                25,
                ETHEREUM,
                SPOKE,
                SOLANA,
                HUB,
                SOLANA_HUB,
                &transfer_payload(DECIMALS, AMOUNT, SOLANA),
            ),
            VaaAccounts::registered(ETHEREUM, SPOKE, SOLANA, HUB, SOLANA_HUB),
            None,
            GlobalAccountantError::BalanceUnderflow,
        ),
        (
            "body_len mismatch",
            hub_to_spoke(26),
            hub_to_spoke_accounts(),
            Some(1),
            GlobalAccountantError::InvalidInstructionData,
        ),
        (
            "truncated transfer",
            VaaScenario::direct(27, SOLANA, HUB, ETHEREUM, SPOKE, SOLANA_HUB, &truncated),
            hub_to_spoke_accounts(),
            None,
            GlobalAccountantError::MalformedNttMessage,
        ),
        (
            "payload over the cap",
            VaaScenario::direct(28, SOLANA, HUB, ETHEREUM, SPOKE, SOLANA_HUB, &oversized),
            hub_to_spoke_accounts(),
            None,
            GlobalAccountantError::NttPayloadTooLarge,
        ),
        (
            "non-canonical hub pda",
            spoofed_hub,
            hub_to_spoke_accounts(),
            None,
            GlobalAccountantError::InvalidPda,
        ),
        (
            "non-canonical source balance pda",
            spoofed_source,
            hub_to_spoke_accounts(),
            None,
            GlobalAccountantError::InvalidAccountPda,
        ),
        (
            "relayed envelope from an unregistered relayer",
            VaaScenario::relayed(
                35,
                ETHEREUM,
                RELAYER,
                SPOKE,
                SOLANA,
                HUB,
                SOLANA_HUB,
                &transfer_payload(DECIMALS, AMOUNT, SOLANA),
            ),
            VaaAccounts {
                relayer_registration: uninitialised_pda_account(),
                ..relayed_accounts()
            },
            None,
            GlobalAccountantError::MalformedNttMessage,
        ),
        (
            "registered relayer without an envelope",
            no_envelope,
            relayed_accounts(),
            None,
            GlobalAccountantError::MalformedDeliveryInstruction,
        ),
    ];
    for (label, transfer, accounts, body_len_delta, expected) in cases {
        let before = transfer.keyed(accounts.clone());
        let result = match body_len_delta {
            Some(delta) => transfer.submit_with(
                &mollusk,
                accounts,
                submit_vaas_ix_data_with_len(
                    transfer.vaa.guardian_set_bump,
                    transfer.vaa.body.len() as u16 + delta,
                    &transfer.vaa.body,
                ),
            ),
            None => transfer.submit(&mollusk, accounts),
        };
        assert_error(&result, expected as u64, label);
        for (key, account) in &before {
            assert_eq!(
                find_account(&result.resulting_accounts, key),
                account,
                "{label}: {key} unchanged"
            );
        }
    }
}

/// A synthetic peer for the corpus rows; the corpus has hubs but no peer table.
const CORPUS_PEER: [u8; 32] = [0xEEu8; 32];

fn hex_bytes(s: &str) -> Vec<u8> {
    let s = s.strip_prefix("0x").unwrap_or(s);
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("hex"))
        .collect()
}

fn hex32(s: &str) -> [u8; 32] {
    hex_bytes(s).try_into().expect("32 bytes")
}

fn u16_field(v: &serde_json::Value, k: &str) -> u16 {
    u16::try_from(v[k].as_u64().expect(k)).expect(k)
}

/// Every mainnet vector, re-signed by the test guardian set, books `expected_amount` under
/// the hub wormchain recorded for its sender.
#[test]
fn mainnet_vectors_commit_through_submit_vaas() {
    let mollusk = mollusk();
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
    assert_eq!(vectors.len(), 28, "full corpus");
    for v in vectors {
        let chain = u16_field(v, "chain");
        let sequence = v["sequence"].as_u64().expect("sequence");
        let label = format!("chain={chain} seq={sequence}");
        let emitter = hex32(v["emitter"].as_str().expect("emitter"));
        let via_relayer = v["via_relayer"].as_bool().expect("via_relayer");
        let recipient_chain = u16_field(v, "expected_recipient_chain");
        let amount = Uint256::from_be_bytes(hex32(v["expected_amount"].as_str().unwrap()));

        let vaa = hex_bytes(v["vaa_hex"].as_str().expect("vaa_hex"));
        let body = vaa[6 + 66 * vaa[5] as usize..].to_vec();
        let sender = if via_relayer {
            parse_delivery_instruction(&body[51..])
                .expect("delivery")
                .sender
        } else {
            emitter
        };
        let hub = hubs[&(chain, sender)];
        assert_eq!(
            hub,
            (
                u16_field(v, "expected_token_chain"),
                hex32(v["expected_token_address"].as_str().unwrap()),
            ),
            "[{label}] corpus hub is the committed token"
        );

        let transfer = VaaScenario::build(
            sequence,
            chain,
            emitter,
            sender,
            recipient_chain,
            CORPUS_PEER,
            hub,
            body,
        );
        let source_debited = chain != hub.0;
        let dest_debited = recipient_chain == hub.0;
        let funded = |debited: bool, on_chain: u16| {
            if debited {
                balance_account(on_chain, hub.0, hub.1, amount)
            } else {
                uninitialised_pda_account()
            }
        };
        let accounts = VaaAccounts {
            relayer_registration: if via_relayer {
                chain_registration_account_for(&program_id(), chain, emitter)
            } else {
                uninitialised_pda_account()
            },
            source_balance: funded(source_debited, chain),
            dest_balance: funded(dest_debited, recipient_chain),
            ..VaaAccounts::registered(chain, sender, recipient_chain, CORPUS_PEER, hub)
        };

        let result = transfer.submit(&mollusk, accounts);
        assert_success(&result, &label);
        let after = &result.resulting_accounts;
        assert_bucket_marked(find_account(after, &transfer.noreplay_bucket), sequence);
        let settled = |debited: bool| if debited { Uint256::ZERO } else { amount };
        assert_balance(after, &transfer.source_balance, settled(source_debited));
        assert_balance(after, &transfer.dest_balance, settled(dest_debited));
    }
}
