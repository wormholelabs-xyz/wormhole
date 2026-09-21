//! `submit_observations`: guardian observations accumulate per `(chain, emitter, sequence,
//! guardian set, digest)` and commit at quorum, routed by the sender's hub.

use std::collections::HashMap;

use global_accountant_definitions::{
    parse_delivery_instruction, GlobalAccountantError, NativeTokenTransfer,
    PendingObservationsLayout, Uint256, SUBMIT_OBSERVATION_PREFIX,
};
use mollusk_svm::Mollusk;
use solana_account::Account;
use solana_instruction::AccountMeta;
use solana_pubkey::Pubkey;

use crate::common::*;

const SOLANA_HUB: (u16, [u8; 32]) = (SOLANA, HUB);
/// 1.5 tokens at 6 decimals; the accountant books it at 8.
const DECIMALS: u8 = 6;
const AMOUNT: u64 = 1_500_000;
const BOOKED: u128 = 150_000_000;
/// Mollusk clock value for expiry rows.
const NOW: i64 = 1_800_000_000;

fn pending_layout(account: &Account) -> PendingObservationsLayout {
    *bytemuck::from_bytes::<PendingObservationsLayout>(&account.data)
}

fn assert_closed(account: &Account, label: &str) {
    assert_eq!(account.lamports, 0, "{label}: lamports");
    assert_eq!(account.owner, system_program_id(), "{label}: owner");
    assert!(account.data.is_empty(), "{label}: data");
}

/// The Solana hub sends to its Ethereum spoke.
fn hub_to_spoke(sequence: u64) -> ObsScenario {
    ObsScenario::new(
        Observation::direct(SOLANA, HUB, sequence, ETHEREUM, DECIMALS, AMOUNT),
        SPOKE,
        SOLANA_HUB,
    )
}

/// The Ethereum spoke sends to the hub through the Standard Relayer.
fn relayed_spoke_to_hub(sequence: u64) -> ObsScenario {
    ObsScenario::new(
        Observation::relayed(ETHEREUM, RELAYER, SPOKE, sequence, SOLANA, DECIMALS, AMOUNT),
        HUB,
        SOLANA_HUB,
    )
}

#[test]
fn quorum_commit_routes_by_hub_marks_noreplay_and_refunds_payer() {
    let mollusk = mollusk();
    let s = hub_to_spoke(6);

    let after_one = s.submit_n(&mollusk, 1);
    let pending = find_account(&after_one, &s.pending_pda);
    assert_eq!(pending.owner, program_id());
    let mut expected = PendingObservationsLayout::new(
        s.obs.chain,
        s.guardian_set_index,
        s.obs.content_digest(),
        SUBMITTER.to_bytes(),
    );
    expected.signatures = [0b1, 0, 0, 0];
    assert_eq!(pending_layout(pending), expected);
    assert_bucket_unmarked(find_account(&after_one, &s.noreplay_bucket));

    let after_twelve = s.submit_range(&mollusk, after_one, 1..12);
    let pending = find_account(&after_twelve, &s.pending_pda);
    assert_eq!(
        pending_layout(pending).signatures,
        [0b1111_1111_1111, 0, 0, 0]
    );
    assert_bucket_unmarked(find_account(&after_twelve, &s.noreplay_bucket));
    assert_eq!(
        find_account(&after_twelve, &s.source_balance).owner,
        system_program_id()
    );
    let pending_rent = pending.lamports;
    let payer_before = find_account(&after_twelve, &SUBMITTER).lamports;

    let bob = Pubkey::new_from_array([0xB0u8; 32]);
    let mut accounts = after_twelve.clone();
    accounts.push((bob, system_owned_account(50_000_000_000)));
    let mut metas = s.account_metas();
    metas[0] = AccountMeta::new(bob, true);
    metas[9] = AccountMeta::new(bob, false);
    let wrong_recipient = s.submit_with(&mollusk, accounts.clone(), s.ix_data(12), metas.clone());
    assert_error(
        &wrong_recipient,
        GlobalAccountantError::PayerMismatch as u64,
        "rent recipient must be the recorded payer",
    );

    metas[9] = AccountMeta::new(SUBMITTER, false);
    let committed = s.submit_with(&mollusk, accounts, s.ix_data(12), metas);
    assert_success(&committed, "quorum commit");
    let after = &committed.resulting_accounts;
    assert_closed(find_account(after, &s.pending_pda), "pending closed");
    assert_bucket_marked(find_account(after, &s.noreplay_bucket), s.obs.sequence);
    assert_eq!(
        find_account(after, &SUBMITTER).lamports,
        payer_before + pending_rent,
        "recorded payer refunded"
    );
    assert_balance(after, &s.source_balance, Uint256::from_u128(BOOKED));
    assert_balance(after, &s.dest_balance, Uint256::from_u128(BOOKED));
}

#[test]
fn relayed_observation_commits_under_the_inner_sender() {
    let mollusk = mollusk();
    let s = relayed_spoke_to_hub(7);
    let mut accounts = s.initial_accounts();
    replace_account(
        &mut accounts,
        &s.source_balance,
        balance_account(ETHEREUM, SOLANA, HUB, Uint256::from_u128(BOOKED)),
    );
    replace_account(
        &mut accounts,
        &s.dest_balance,
        balance_account(SOLANA, SOLANA, HUB, Uint256::from_u128(BOOKED)),
    );
    let after = s.submit_range(&mollusk, accounts, 0..13);
    assert_closed(find_account(&after, &s.pending_pda), "pending closed");
    assert_bucket_marked(find_account(&after, &s.noreplay_bucket), s.obs.sequence);
    assert_balance(&after, &s.source_balance, Uint256::ZERO);
    assert_balance(&after, &s.dest_balance, Uint256::ZERO);
}

/// `tx_hash` is in the signing digest but not in the content digest, so observations of one
/// message under different `tx_hash` values share a pending PDA.
#[test]
fn tx_hash_does_not_split_the_pending_pda() {
    let mollusk = mollusk();
    let s = hub_to_spoke(8);
    let first = s.submit_once(&mollusk, s.initial_accounts(), 0);
    assert_success(&first, "guardian 0, default tx_hash");

    let other_tx_hash = [0x5Au8; 32];
    let signature = sign_digest(
        &s.guardians[1],
        &s.obs.signing_digest_with(
            global_accountant_definitions::NTT_SUBMIT_OBSERVATION_PREFIX,
            &other_tx_hash,
        ),
    );
    let second = s.submit_with(
        &mollusk,
        first.resulting_accounts,
        s.ix_data_signed(1, signature, &other_tx_hash),
        s.account_metas(),
    );
    assert_success(&second, "guardian 1, other tx_hash");
    let pending = pending_layout(find_account(&second.resulting_accounts, &s.pending_pda));
    assert_eq!(
        pending.num_signatures(),
        2,
        "both observations in one pending PDA"
    );
}

#[test]
fn rejects() {
    let mut mollusk = mollusk();
    mollusk.sysvars.clock.unix_timestamp = NOW;
    type Case = fn(&Mollusk) -> (ObsScenario, Vec<(Pubkey, Account)>, Vec<u8>);

    let cases: [(&str, Case, GlobalAccountantError); 13] = [
        (
            "sender differs from emitter with no relayer registered",
            |_| {
                let s = relayed_spoke_to_hub(20);
                let mut accounts = s.initial_accounts();
                replace_account(
                    &mut accounts,
                    &s.relayer_registration_pda,
                    uninitialised_pda_account(),
                );
                let ix = s.ix_data(0);
                (s, accounts, ix)
            },
            GlobalAccountantError::UnregisteredEmitter,
        ),
        (
            "registered relayer with sender equal to emitter",
            |_| {
                let s = ObsScenario::new(
                    Observation::direct(ETHEREUM, RELAYER, 21, SOLANA, DECIMALS, AMOUNT),
                    HUB,
                    SOLANA_HUB,
                );
                let mut accounts = s.initial_accounts();
                replace_account(
                    &mut accounts,
                    &s.relayer_registration_pda,
                    chain_registration_account_for(&program_id(), ETHEREUM, RELAYER),
                );
                let ix = s.ix_data(0);
                (s, accounts, ix)
            },
            GlobalAccountantError::InvalidInstructionData,
        ),
        (
            "missing hub rejected before signature recovery",
            |_| {
                let s = hub_to_spoke(22);
                let mut accounts = s.initial_accounts();
                replace_account(&mut accounts, &s.hub_pda, uninitialised_pda_account());
                let ix = s.ix_data_signed(0, [0x11; 65], &TX_HASH);
                (s, accounts, ix)
            },
            GlobalAccountantError::MissingTransceiverHub,
        ),
        (
            "corrupted signature",
            |_| {
                let s = hub_to_spoke(23);
                let mut sig = sign_digest(&s.guardians[0], &s.obs.signing_digest());
                sig[0] ^= 0xff;
                let accounts = s.initial_accounts();
                let ix = s.ix_data_signed(0, sig, &TX_HASH);
                (s, accounts, ix)
            },
            GlobalAccountantError::InvalidSignature,
        ),
        (
            "signature over the WTT prefix",
            |_| {
                let s = hub_to_spoke(24);
                let digest = s
                    .obs
                    .signing_digest_with(SUBMIT_OBSERVATION_PREFIX, &TX_HASH);
                assert_ne!(digest, s.obs.signing_digest());
                let sig = sign_digest(&s.guardians[0], &digest);
                let accounts = s.initial_accounts();
                let ix = s.ix_data_signed(0, sig, &TX_HASH);
                (s, accounts, ix)
            },
            GlobalAccountantError::InvalidSignature,
        ),
        (
            "218 bytes",
            |_| {
                let s = hub_to_spoke(25);
                let accounts = s.initial_accounts();
                let mut ix = s.ix_data(0);
                ix.pop();
                (s, accounts, ix)
            },
            GlobalAccountantError::InvalidInstructionData,
        ),
        (
            "220 bytes",
            |_| {
                let s = hub_to_spoke(26);
                let accounts = s.initial_accounts();
                let mut ix = s.ix_data(0);
                ix.push(0);
                (s, accounts, ix)
            },
            GlobalAccountantError::InvalidInstructionData,
        ),
        (
            "expired guardian set",
            |_| {
                let s = hub_to_spoke(27);
                let mut accounts = s.initial_accounts();
                replace_account(
                    &mut accounts,
                    &s.guardian_set,
                    guardian_set_account(
                        s.guardian_set_index,
                        &guardian_keys(&s.guardians),
                        0,
                        NOW as u32 - 1,
                        &core_bridge_program_id(),
                    ),
                );
                let ix = s.ix_data(0);
                (s, accounts, ix)
            },
            GlobalAccountantError::ExpiredGuardianSet,
        ),
        (
            "pre-marked noreplay",
            |_| {
                let s = hub_to_spoke(28);
                let mut accounts = s.initial_accounts();
                replace_account(
                    &mut accounts,
                    &s.noreplay_bucket,
                    noreplay_bucket_marked(28),
                );
                let ix = s.ix_data(0);
                (s, accounts, ix)
            },
            GlobalAccountantError::AlreadyAccounted,
        ),
        (
            "peer not cross-registered at quorum",
            |m| {
                let s = hub_to_spoke(29);
                let mut accounts = s.submit_n(m, 12);
                replace_account(
                    &mut accounts,
                    &s.peer_dst_pda,
                    peer_account(&peer_layout(ETHEREUM, SPOKE, SOLANA, OTHER)),
                );
                let ix = s.ix_data(12);
                (s, accounts, ix)
            },
            GlobalAccountantError::PeersNotCrossRegistered,
        ),
        (
            "trimmed decimals past the normalizer at quorum",
            |m| {
                let s = ObsScenario::new(
                    Observation::direct(SOLANA, HUB, 30, ETHEREUM, 86, AMOUNT),
                    SPOKE,
                    SOLANA_HUB,
                );
                let accounts = s.submit_n(m, 12);
                let ix = s.ix_data(12);
                (s, accounts, ix)
            },
            GlobalAccountantError::InvalidInstructionData,
        ),
        (
            "wrong source balance pda at quorum",
            |m| {
                let mut s = hub_to_spoke(31);
                let mut accounts = s.submit_n(m, 12);
                s.source_balance = Pubkey::new_unique();
                accounts.push((s.source_balance, uninitialised_pda_account()));
                let ix = s.ix_data(12);
                (s, accounts, ix)
            },
            GlobalAccountantError::InvalidAccountPda,
        ),
        (
            "wrapped source underflow at quorum",
            |m| {
                let s = ObsScenario::new(
                    Observation::direct(ETHEREUM, SPOKE, 32, SOLANA, DECIMALS, AMOUNT),
                    HUB,
                    SOLANA_HUB,
                );
                let accounts = s.submit_n(m, 12);
                let ix = s.ix_data(12);
                (s, accounts, ix)
            },
            GlobalAccountantError::BalanceUnderflow,
        ),
    ];
    for (label, case, expected) in cases {
        let (s, accounts, ix_data) = case(&mollusk);
        let before = accounts.clone();
        let result = s.submit_with(&mollusk, accounts, ix_data, s.account_metas());
        assert_error(&result, expected as u64, label);
        assert_eq!(
            result.resulting_accounts, before,
            "{label}: state untouched"
        );
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

/// Every mainnet vector, observed by thirteen test guardians with the fields the node would
/// extract, books `expected_amount` under the hub wormchain recorded for its sender.
#[test]
fn mainnet_vectors_commit_at_quorum() {
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
        let recipient_chain = u16_field(v, "expected_recipient_chain");
        let expected = Uint256::from_be_bytes(hex32(v["expected_amount"].as_str().unwrap()));

        let vaa = hex_bytes(v["vaa_hex"].as_str().expect("vaa_hex"));
        let body = &vaa[6 + 66 * vaa[5] as usize..];
        let payload = &body[51..];
        let (sender, ntt_payload) = if v["via_relayer"].as_bool().expect("via_relayer") {
            let d = parse_delivery_instruction(payload).expect("delivery");
            (d.sender, d.inner_payload)
        } else {
            (emitter, payload)
        };
        // `NativeTokenTransfer` follows the 70-byte transceiver head and 66-byte manager head.
        let transfer: &NativeTokenTransfer = bytemuck::from_bytes(&ntt_payload[136..215]);
        let hub = hubs[&(chain, sender)];

        let obs = Observation {
            chain,
            emitter,
            sequence,
            sender,
            recipient_chain,
            trimmed_decimals: transfer.decimals,
            trimmed_amount: u64::from_be_bytes(transfer.amount),
            digest: double_keccak256(body),
        };
        let s = ObsScenario::new(obs, CORPUS_PEER, hub);
        let source_debited = chain != hub.0;
        let dest_debited = recipient_chain == hub.0;
        let mut accounts = s.initial_accounts();
        if source_debited {
            replace_account(
                &mut accounts,
                &s.source_balance,
                balance_account(chain, hub.0, hub.1, expected),
            );
        }
        if dest_debited {
            replace_account(
                &mut accounts,
                &s.dest_balance,
                balance_account(recipient_chain, hub.0, hub.1, expected),
            );
        }

        let after = s.submit_range(&mollusk, accounts, 0..13);
        assert_closed(find_account(&after, &s.pending_pda), &label);
        assert_bucket_marked(find_account(&after, &s.noreplay_bucket), sequence);
        let settled = |debited: bool| if debited { Uint256::ZERO } else { expected };
        assert_balance(&after, &s.source_balance, settled(source_debited));
        assert_balance(&after, &s.dest_balance, settled(dest_debited));
    }
}
