//! Cross-`.so` checks on the shared PDA path: state written by the backfill `.so` is
//! accepted by the NTT operational `.so` running at the same program id. The accounts cross
//! from one binary to the other untouched, so `register_peer` reaches its hub-match arm and
//! `submit_vaas` settles a mainnet transfer against backfilled maps and balances.

use accountant_operational_core::accounts::{balance, chain_registration};
use accountant_operational_core::cpi::noreplay::derive_bucket_pda;
use accountant_operational_core::instructions::register_chain::derive_register_chain_pda;
use accountant_operational_core::support::pda;
use accountant_test_fixtures::NttCorpus;
use accountant_test_harness::mollusk_with_fixtures;
use global_accountant_definitions::ntt_global_accountant_backfill::Instruction;
use global_accountant_definitions::{
    parse_delivery_instruction, TransceiverHubKey, TransceiverHubLayout, TransceiverPeerKey,
    TransceiverPeerLayout, Uint256, VaaBodyHeader,
};
use mollusk_svm::program::keyed_account_for_system_program;
use mollusk_svm::Mollusk;
use solana_account::Account;
use solana_instruction::{AccountMeta, Instruction as SolanaInstruction};
use solana_pubkey::Pubkey;

use crate::common::*;
use crate::ntt_ix::{direct_body, peer_payload, register_peer_ix_data, submit_vaas_ix_data};

/// The NTT operational program's `.so` under `SBF_OUT_DIR`, at the shared program id.
const OPERATIONAL_PROGRAM_NAME: &str = "ntt_global_accountant";

const SEQUENCE: u64 = 6;

/// Step 5 of the CosmWasm setup order: the hub pre-registers a hubless spoke, which needs
/// the hub's own `TransceiverHub` entry to exist and to name itself.
#[test]
fn backfilled_hub_satisfies_operational_register_peer() {
    let id = program_id();
    let hub_pda = pda::derive(&id, &TransceiverHubKey::new(SOLANA, HUB)).0;

    // Phase 1: the backfill `.so` writes the hub entry.
    let entry = wire::transceiver_hub_entry(SOLANA, HUB, SOLANA, HUB);
    let signer = ntt_test_authority_pubkey();
    let backfill_result = mollusk().process_instruction(
        &SolanaInstruction::new_with_bytes(
            id,
            &wire::encode_transceiver_hub_batch(
                Instruction::BackfillTransceiverHub as u8,
                &[entry],
            ),
            vec![
                AccountMeta::new(signer, true),
                AccountMeta::new_readonly(system_program_id(), false),
                AccountMeta::new(hub_pda, false),
            ],
        ),
        &[
            (signer, system_owned_account(10_000_000_000)),
            keyed_account_for_system_program(),
            (hub_pda, uninitialised_pda_account()),
        ],
    );
    assert_success(&backfill_result, "backfill hub");
    let backfilled_hub = find_account(&backfill_result.resulting_accounts, &hub_pda).clone();
    assert_eq!(
        layout::<TransceiverHubLayout>(&backfilled_hub),
        TransceiverHubLayout::new(
            TransceiverHubKey::new(SOLANA, HUB),
            TransceiverHubKey::new(SOLANA, HUB),
        ),
        "backfilled hub layout"
    );

    // Phase 2: the operational `.so`, at the same id, registers the spoke against it.
    let peer_entry_key = TransceiverPeerKey::new(SOLANA, HUB, ETHEREUM);
    let peer_pda = pda::derive(&id, &peer_entry_key).0;
    let peer_hub_pda = pda::derive(&id, &TransceiverHubKey::new(ETHEREUM, SPOKE)).0;
    let hub_peer_pda = pda::derive(&id, &TransceiverPeerKey::new(ETHEREUM, SPOKE, SOLANA)).0;
    let relayer_registration_pda =
        accountant_operational_core::accounts::chain_registration::derive_pda(&id, SOLANA).0;
    let noreplay_authority = noreplay_authority_pda(&id);
    let noreplay_bucket = derive_bucket_pda(&noreplay_authority, SOLANA, &HUB, SEQUENCE).0;

    let vaa = SignedVaa::new(direct_body(
        SOLANA,
        HUB,
        SEQUENCE,
        &peer_payload(ETHEREUM, SPOKE),
    ));
    let operational = mollusk_with_fixtures(&id, OPERATIONAL_PROGRAM_NAME);
    let result = vaa.submit(
        &operational,
        id,
        &register_peer_ix_data(vaa.guardian_set_bump, &vaa.body),
        vec![
            AccountMeta::new_readonly(relayer_registration_pda, false),
            AccountMeta::new(hub_pda, false),
            AccountMeta::new_readonly(peer_hub_pda, false),
            AccountMeta::new_readonly(hub_peer_pda, false),
            AccountMeta::new(peer_pda, false),
            AccountMeta::new(noreplay_bucket, false),
            AccountMeta::new_readonly(noreplay_program_id(), false),
            AccountMeta::new_readonly(noreplay_authority, false),
            AccountMeta::new_readonly(system_program_id(), false),
        ],
        vec![
            (relayer_registration_pda, uninitialised_pda_account()),
            (hub_pda, backfilled_hub.clone()),
            (peer_hub_pda, uninitialised_pda_account()),
            (hub_peer_pda, uninitialised_pda_account()),
            (peer_pda, uninitialised_pda_account()),
            (noreplay_bucket, noreplay_bucket_unmarked()),
            keyed_account_for_noreplay_program(),
            (noreplay_authority, system_owned_account(0)),
            keyed_account_for_system_program(),
        ],
    );
    assert_success(&result, "operational register_peer over the backfilled hub");

    let peer = find_account(&result.resulting_accounts, &peer_pda);
    assert_eq!(peer.owner, id, "peer owner");
    assert_eq!(
        layout::<TransceiverPeerLayout>(peer),
        TransceiverPeerLayout::new(peer_entry_key, SPOKE),
        "peer layout"
    );
    assert_eq!(
        find_account(&result.resulting_accounts, &hub_pda),
        &backfilled_hub,
        "the hub entry is read, not rewritten"
    );
}

/// Peer address registered for every corpus sender: wormchain's dump carries the hub map,
/// not the peer map, so the counterparty transceiver is a stand-in.
const CORPUS_PEER: [u8; 32] = [0xEEu8; 32];

/// One direct and one relayed mainnet transfer from `NTT_TEST_VECTORS`, by `(chain, sequence)`.
const PARITY_VECTORS: [(u16, u64); 2] = [(ETHEREUM, 5), (ETHEREUM, 13444)];

/// Governance sequence the relayer registration is backfilled under, as `register_chain`
/// writes it.
const RELAYER_REGISTRATION_SEQUENCE: u64 = 0;

/// Run one backfill instruction and return the PDAs it wrote, in `pdas` order.
fn backfill(
    mollusk: &Mollusk,
    data: &[u8],
    pdas: &[Pubkey],
    label: &str,
) -> Vec<(Pubkey, Account)> {
    let signer = ntt_test_authority_pubkey();
    let mut metas = vec![
        AccountMeta::new(signer, true),
        AccountMeta::new_readonly(system_program_id(), false),
    ];
    let mut accounts = vec![
        (signer, system_owned_account(10_000_000_000)),
        keyed_account_for_system_program(),
    ];
    for pda_key in pdas {
        metas.push(AccountMeta::new(*pda_key, false));
        accounts.push((*pda_key, uninitialised_pda_account()));
    }
    let result = mollusk.process_instruction(
        &SolanaInstruction::new_with_bytes(program_id(), data, metas),
        &accounts,
    );
    assert_success(&result, label);
    pdas.iter()
        .map(|key| (*key, find_account(&result.resulting_accounts, key).clone()))
        .collect()
}

/// Cutover acceptance: the hub, peer, balance and relayer state the backfill `.so` writes is
/// everything the operational `.so` needs to settle a real mainnet transfer. Each vector is
/// the wormchain-committed body, re-signed by the test guardian set, and the balances move by
/// the amount wormchain booked.
#[test]
fn backfilled_state_settles_mainnet_transfers_through_submit_vaas() {
    let id = program_id();
    let corpus = NttCorpus::load();
    let backfill_mollusk = mollusk();
    let operational = mollusk_with_fixtures(&id, OPERATIONAL_PROGRAM_NAME);

    for (chain, sequence) in PARITY_VECTORS {
        let v = corpus
            .vectors
            .iter()
            .find(|v| v.chain == chain && v.sequence == sequence)
            .expect("corpus vector");
        let label = v.label();
        let body = v.body();
        let (_, payload) = VaaBodyHeader::split(body).expect("header");
        let sender = if v.via_relayer {
            parse_delivery_instruction(payload)
                .expect("delivery")
                .sender
        } else {
            v.emitter
        };
        let hub = corpus.hub_for(v.chain, sender).expect("hub");
        let amount = Uint256::from_be_bytes(v.expected_amount);
        let recipient_chain = v.expected_recipient_chain;

        let hub_pda = pda::derive(&id, &TransceiverHubKey::new(v.chain, sender)).0;
        let peer_src_pda = pda::derive(
            &id,
            &TransceiverPeerKey::new(v.chain, sender, recipient_chain),
        )
        .0;
        let peer_dst_pda = pda::derive(
            &id,
            &TransceiverPeerKey::new(recipient_chain, CORPUS_PEER, v.chain),
        )
        .0;
        let source_balance = balance::derive_pda(&id, v.chain, hub.0, &hub.1).0;
        let dest_balance = balance::derive_pda(&id, recipient_chain, hub.0, &hub.1).0;
        let relayer_registration_pda = chain_registration::derive_pda(&id, v.chain).0;
        let noreplay_authority = noreplay_authority_pda(&id);
        let noreplay_bucket =
            derive_bucket_pda(&noreplay_authority, v.chain, &v.emitter, v.sequence).0;

        // Phase 1: the backfill `.so` seeds the snapshot state this transfer reads.
        let hub_account = backfill(
            &backfill_mollusk,
            &wire::encode_transceiver_hub_batch(
                Instruction::BackfillTransceiverHub as u8,
                &[wire::transceiver_hub_entry(v.chain, sender, hub.0, hub.1)],
            ),
            &[hub_pda],
            &format!("[{label}] backfill hub"),
        )[0]
        .1
        .clone();

        let mut peer_entries = [
            wire::transceiver_peer_entry(v.chain, sender, recipient_chain, CORPUS_PEER),
            wire::transceiver_peer_entry(recipient_chain, CORPUS_PEER, v.chain, sender),
        ];
        peer_entries.sort_by_key(|entry| (entry.chain(), entry.address, entry.dest_chain()));
        let peer_pdas: Vec<Pubkey> = peer_entries
            .iter()
            .map(|entry| pda::derive(&id, &entry.key()).0)
            .collect();
        let peers = backfill(
            &backfill_mollusk,
            &wire::encode_transceiver_peer_batch(
                Instruction::BackfillTransceiverPeer as u8,
                &peer_entries,
            ),
            &peer_pdas,
            &format!("[{label}] backfill peers"),
        );

        // The hub chain holds the native token, so it is debited on the way out and
        // credited on the way in; the wrapped side moves the other way.
        let source_debited = v.chain != hub.0;
        let dest_debited = recipient_chain == hub.0;
        let mut balance_entries: Vec<_> =
            [(v.chain, source_debited), (recipient_chain, dest_debited)]
                .into_iter()
                .filter(|(_, debited)| *debited)
                .map(|(on_chain, _)| wire::balance_entry(on_chain, hub.0, hub.1, v.expected_amount))
                .collect();
        balance_entries.sort_by_key(|entry| entry.sort_key());
        let balance_pdas: Vec<Pubkey> = balance_entries
            .iter()
            .map(|entry| {
                balance::derive_pda(
                    &id,
                    entry.chain(),
                    entry.token_chain(),
                    &entry.token_address,
                )
                .0
            })
            .collect();
        assert!(!balance_entries.is_empty(), "[{label}] a funded side");
        let balances = backfill(
            &backfill_mollusk,
            &wire::encode_balance_batch(Instruction::BackfillBalance as u8, &balance_entries),
            &balance_pdas,
            &format!("[{label}] backfill balances"),
        );
        let funded = |key: &Pubkey| {
            balances
                .iter()
                .find(|(k, _)| k == key)
                .map_or_else(uninitialised_pda_account, |(_, account)| account.clone())
        };

        // A relayed transfer resolves its sender through the Standard Relayer registration.
        let relayer_registration = if v.via_relayer {
            let record_pda = derive_register_chain_pda(&id, RELAYER_REGISTRATION_SEQUENCE).0;
            backfill(
                &backfill_mollusk,
                &wire::encode_chain_registration_batch(
                    Instruction::BackfillRelayerChainRegistration as u8,
                    &[wire::chain_registration_entry(
                        v.chain,
                        RELAYER_REGISTRATION_SEQUENCE,
                        v.emitter,
                    )],
                ),
                &[relayer_registration_pda, record_pda],
                &format!("[{label}] backfill relayer registration"),
            )[0]
            .1
            .clone()
        } else {
            uninitialised_pda_account()
        };

        // Phase 2: the operational `.so`, at the same id, settles the transfer.
        let vaa = SignedVaa::new(body.to_vec());
        let result = vaa.submit(
            &operational,
            id,
            &submit_vaas_ix_data(vaa.guardian_set_bump, &vaa.body),
            vec![
                AccountMeta::new(noreplay_bucket, false),
                AccountMeta::new_readonly(noreplay_program_id(), false),
                AccountMeta::new_readonly(noreplay_authority, false),
                AccountMeta::new(source_balance, false),
                AccountMeta::new(dest_balance, false),
                AccountMeta::new_readonly(system_program_id(), false),
                AccountMeta::new_readonly(relayer_registration_pda, false),
                AccountMeta::new_readonly(hub_pda, false),
                AccountMeta::new_readonly(peer_src_pda, false),
                AccountMeta::new_readonly(peer_dst_pda, false),
            ],
            vec![
                (noreplay_bucket, noreplay_bucket_unmarked()),
                keyed_account_for_noreplay_program(),
                (noreplay_authority, system_owned_account(0)),
                (source_balance, funded(&source_balance)),
                (dest_balance, funded(&dest_balance)),
                keyed_account_for_system_program(),
                (relayer_registration_pda, relayer_registration),
                (hub_pda, hub_account.clone()),
                (peer_src_pda, find_account(&peers, &peer_src_pda).clone()),
                (peer_dst_pda, find_account(&peers, &peer_dst_pda).clone()),
            ],
        );
        assert_success(&result, &label);

        let after = &result.resulting_accounts;
        assert_bucket_marked(find_account(after, &noreplay_bucket), v.sequence);
        let settled = |debited: bool| if debited { Uint256::ZERO } else { amount };
        assert_balance_for(&id, after, &source_balance, settled(source_debited));
        assert_balance_for(&id, after, &dest_balance, settled(dest_debited));
        assert_eq!(
            find_account(after, &hub_pda),
            &hub_account,
            "[{label}] the backfilled hub is read, not rewritten"
        );
        for (key, account) in &peers {
            assert_eq!(
                find_account(after, key),
                account,
                "[{label}] the backfilled peer {key} is read, not rewritten"
            );
        }
    }
}
