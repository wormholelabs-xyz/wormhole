//! Cutover rehearsal for design choice 2: the backfill `.so` and the operational `.so` share
//! one program account, and the migration ends with `solana program upgrade`.
//!
//! Shape: one `#[ignore]` test in one surfpool process. `upgrade_contract` is an instruction
//! *of the running program*, so the governance upgrade path opens once the operational image
//! is live. The cutover itself is the operator's `solana program upgrade`, so the rehearsal
//! sends the loader's `Upgrade` directly with the deploy key as upgrade authority.
//! `crates/test-harness/src/upgrade_e2e.rs`, the detached two-step governance flow, stays
//! with the operational program's own `upgrade_contract` suite.
//!
//! Phases:
//!
//! 1. The backfill `.so`, at the NTT program id, seeds the state one mainnet transfer reads:
//!    its hub, both peers and both balances, plus the NoReplay bit of a second mainnet
//!    transfer from the same emitter.
//! 2. The loader swaps in `ntt_global_accountant.so` at the same address.
//! 3. The operational `.so` settles the first transfer through `submit_vaas`: the balances
//!    move by the amount wormchain booked, the NoReplay mark lands in the bucket the backfill
//!    created, and the backfilled hub and peers come back byte-identical.
//! 4. `submit_vaas` on the second transfer rejects with `AlreadyAccounted`, so a sequence
//!    wormchain already consumed cannot be replayed after the cutover.
//!
//! The VAA bodies are the real mainnet bytes; the signatures are the test guardian set's,
//! since mainnet signatures cannot verify against a synthetic set.

use accountant_operational_core::accounts::{balance, chain_registration};
use accountant_operational_core::cpi::noreplay::derive_bucket_pda;
use accountant_operational_core::support::pda;
use accountant_test_fixtures::{NttCorpus, NttVector};
use global_accountant_definitions::ntt_global_accountant_backfill::Instruction as Arm;
use global_accountant_definitions::{
    GlobalAccountantError, TransceiverHubKey, TransceiverHubLayout, TransceiverPeerKey,
    TransceiverPeerLayout, Uint256,
};
use solana_account::Account;
use solana_instruction::{AccountMeta, Instruction};
use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use solana_signer::Signer;

use crate::common::*;
use crate::harness::{
    deploy_guardian_set, deploy_programs, fund, loader_upgrade, send, send_expect_error,
    start_surfpool, ProgramImage, SurfpoolOptions,
};
use crate::ntt_ix::submit_vaas_ix_data;

/// The NTT operational program's `.so` under `SBF_OUT_DIR`, deployed at the shared id.
const OPERATIONAL_PROGRAM_NAME: &str = "ntt_global_accountant";

/// Settled after the cutover, by `(chain, sequence)` in `NTT_TEST_VECTORS`.
const SETTLED_VECTOR: (u16, u64) = (2, 5);
/// Backfilled as consumed on wormchain; same emitter as `SETTLED_VECTOR`, so both sequences
/// share one NoReplay bucket.
const CONSUMED_VECTOR: (u16, u64) = (2, 591);

/// Stand-in counterparty transceiver address; the wormchain dump carries the hub map.
const CORPUS_PEER: [u8; 32] = [0xEEu8; 32];

const AUTHORITY_LAMPORTS: u64 = 20_000_000_000;
/// The shim's `verify_vaa` CPI alone consumes ~196k CU with a full quorum.
const SHIM_CU_LIMIT: u32 = 400_000;

#[test]
#[ignore = "spawns surfpool subprocess; run via `just e2e-ntt-cutover`"]
fn surfpool_ntt_cutover_rehearsal() {
    let guard = start_surfpool(SurfpoolOptions::offline("ntt-backfill-cutover"));
    let rpc = guard.rpc_client();

    let backfill = accountant_image();
    let id = backfill.program_id;
    deploy_programs(
        &rpc,
        &[
            backfill,
            ProgramImage::noreplay(),
            ProgramImage::verify_vaa_shim(),
        ],
    );

    // Three roles: the backfill operator, the deploy key that owns the program account, and
    // the permissionless submitter of the post-cutover transfer.
    let operator = ntt_test_authority_keypair();
    let deployer = Keypair::new();
    let payer = Keypair::new();
    for key in [&operator, &deployer, &payer] {
        fund(&rpc, &key.pubkey(), AUTHORITY_LAMPORTS);
    }

    let corpus = NttCorpus::load();
    let vector = |(chain, sequence): (u16, u64)| -> &NttVector {
        let v = corpus
            .vectors
            .iter()
            .find(|v| v.chain == chain && v.sequence == sequence)
            .expect("corpus vector");
        assert!(!v.via_relayer, "{} is a direct vector", v.label());
        v
    };
    let settled = vector(SETTLED_VECTOR);
    let consumed = vector(CONSUMED_VECTOR);

    let hub = corpus
        .hub_for(settled.chain, settled.emitter)
        .expect("corpus hub");
    let recipient_chain = settled.expected_recipient_chain;
    let amount = Uint256::from_be_bytes(settled.expected_amount);
    // The hub chain holds the native token: it is credited on the way in, and the wrapped
    // side burns on the way out, so both sides of this transfer are debited.
    let source_debited = settled.chain != hub.0;
    let dest_debited = recipient_chain == hub.0;

    let hub_pda = pda::derive(&id, &TransceiverHubKey::new(settled.chain, settled.emitter)).0;
    let peer_src_pda = pda::derive(
        &id,
        &TransceiverPeerKey::new(settled.chain, settled.emitter, recipient_chain),
    )
    .0;
    let peer_dst_pda = pda::derive(
        &id,
        &TransceiverPeerKey::new(recipient_chain, CORPUS_PEER, settled.chain),
    )
    .0;
    let source_balance = balance::derive_pda(&id, settled.chain, hub.0, &hub.1).0;
    let dest_balance = balance::derive_pda(&id, recipient_chain, hub.0, &hub.1).0;
    let noreplay_authority = noreplay_authority_pda(&id);
    let bucket =
        |v: &NttVector| derive_bucket_pda(&noreplay_authority, v.chain, &v.emitter, v.sequence).0;

    let write_head = vec![
        AccountMeta::new(operator.pubkey(), true),
        AccountMeta::new_readonly(system_program_id(), false),
    ];
    let backfill_ix = |data: Vec<u8>, pdas: &[Pubkey]| -> Instruction {
        let mut accounts = write_head.clone();
        accounts.extend(pdas.iter().map(|key| AccountMeta::new(*key, false)));
        Instruction {
            program_id: id,
            accounts,
            data,
        }
    };

    // 1. Backfill the state the transfer reads, plus the consumed sequence's NoReplay bit.
    send(
        &rpc,
        "backfill_transceiver_hub",
        &[backfill_ix(
            wire::encode_transceiver_hub_batch(
                Arm::BackfillTransceiverHub as u8,
                &[wire::transceiver_hub_entry(
                    settled.chain,
                    settled.emitter,
                    hub.0,
                    hub.1,
                )],
            ),
            &[hub_pda],
        )],
        &[&operator],
    );

    let mut peer_entries = [
        wire::transceiver_peer_entry(settled.chain, settled.emitter, recipient_chain, CORPUS_PEER),
        wire::transceiver_peer_entry(recipient_chain, CORPUS_PEER, settled.chain, settled.emitter),
    ];
    peer_entries.sort_by_key(|entry| (entry.chain(), entry.address, entry.dest_chain()));
    let peer_pdas: Vec<Pubkey> = peer_entries
        .iter()
        .map(|entry| pda::derive(&id, &entry.key()).0)
        .collect();
    send(
        &rpc,
        "backfill_transceiver_peer",
        &[backfill_ix(
            wire::encode_transceiver_peer_batch(Arm::BackfillTransceiverPeer as u8, &peer_entries),
            &peer_pdas,
        )],
        &[&operator],
    );

    let mut balance_entries: Vec<_> = [
        (settled.chain, source_debited),
        (recipient_chain, dest_debited),
    ]
    .into_iter()
    .filter(|(_, debited)| *debited)
    .map(|(on_chain, _)| wire::balance_entry(on_chain, hub.0, hub.1, settled.expected_amount))
    .collect();
    assert!(!balance_entries.is_empty(), "a funded side");
    balance_entries.sort_by_key(|entry| entry.key());
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
    send(
        &rpc,
        "backfill_balance",
        &[backfill_ix(
            wire::encode_balance_batch(Arm::BackfillBalance as u8, &balance_entries),
            &balance_pdas,
        )],
        &[&operator],
    );

    assert_eq!(
        bucket(consumed),
        bucket(settled),
        "both sequences share one NoReplay bucket, so the backfill creates the account the \
         operational mark lands in"
    );
    let noreplay_accounts = vec![
        AccountMeta::new(operator.pubkey(), true),
        AccountMeta::new_readonly(noreplay_program_id(), false),
        AccountMeta::new_readonly(noreplay_authority, false),
        AccountMeta::new_readonly(system_program_id(), false),
        AccountMeta::new(bucket(consumed), false),
    ];
    send(
        &rpc,
        "backfill_no_replay",
        &[Instruction {
            program_id: id,
            accounts: noreplay_accounts,
            data: wire::encode_noreplay_batch(
                Arm::BackfillNoReplay as u8,
                &[wire::NoReplayEntry {
                    chain: consumed.chain,
                    emitter: consumed.emitter,
                    sequence: consumed.sequence,
                    digest: consumed.expected_digest,
                }],
            ),
        }],
        &[&operator],
    );

    let read = |key: &Pubkey, label: &str| -> Account { rpc.get_account(key).expect(label) };
    let hub_before = read(&hub_pda, "backfilled hub");
    assert_eq!(
        layout::<TransceiverHubLayout>(&hub_before),
        TransceiverHubLayout::new(
            TransceiverHubKey::new(settled.chain, settled.emitter),
            TransceiverHubKey::new(hub.0, hub.1),
        ),
        "backfilled hub layout"
    );
    let peers_before: Vec<(Pubkey, Account)> = peer_entries
        .iter()
        .zip(&peer_pdas)
        .map(|(entry, key)| {
            let account = read(key, "backfilled peer");
            assert_eq!(
                layout::<TransceiverPeerLayout>(&account),
                TransceiverPeerLayout::new(entry.key(), entry.peer_address),
                "backfilled peer layout"
            );
            (*key, account)
        })
        .collect();
    assert_bucket_marked(
        &read(&bucket(consumed), "backfilled bucket"),
        consumed.sequence,
    );

    // 2. The cutover: the operational image takes over the same program account.
    let operational = ProgramImage::from_deploy_dir(OPERATIONAL_PROGRAM_NAME, id);
    loader_upgrade(&rpc, &id, &operational.elf, &deployer);

    // 3. The operational `.so` settles the mainnet transfer against the backfilled state.
    let guardians = make_guardians(GUARDIAN_COUNT, 0x42);
    let (guardian_set, guardian_set_bump) =
        deploy_guardian_set(&rpc, GUARDIAN_SET_INDEX, &guardians);
    let shim_metas = |label: &str, body: &[u8]| -> Vec<AccountMeta> {
        let digest = double_keccak256(body);
        let guardian_signatures = Keypair::new();
        send(
            &rpc,
            &format!("post_signatures[{label}]"),
            &[post_signatures_ix(
                &payer.pubkey(),
                &guardian_signatures.pubkey(),
                GUARDIAN_SET_INDEX,
                QUORUM,
                &signature_block(&signatures_for(&guardians, &digest, QUORUM)),
            )],
            &[&payer, &guardian_signatures],
        );
        vec![
            AccountMeta::new(payer.pubkey(), true),
            AccountMeta::new_readonly(shim_program_id(), false),
            AccountMeta::new_readonly(guardian_set, false),
            AccountMeta::new_readonly(guardian_signatures.pubkey(), false),
        ]
    };
    let submit_vaas_ix = |label: &str, v: &NttVector, tail: Vec<AccountMeta>| -> Vec<Instruction> {
        let body = v.body();
        let mut accounts = shim_metas(label, body);
        accounts.extend(tail);
        vec![
            set_compute_unit_limit_ix(SHIM_CU_LIMIT),
            Instruction {
                program_id: id,
                accounts,
                data: submit_vaas_ix_data(guardian_set_bump, body),
            },
        ]
    };
    let submit_tail = |v: &NttVector,
                       source: Pubkey,
                       dest: Pubkey,
                       hub_key: Pubkey,
                       peer_src: Pubkey,
                       peer_dst: Pubkey| {
        vec![
            AccountMeta::new(bucket(v), false),
            AccountMeta::new_readonly(noreplay_program_id(), false),
            AccountMeta::new_readonly(noreplay_authority, false),
            AccountMeta::new(source, false),
            AccountMeta::new(dest, false),
            AccountMeta::new_readonly(system_program_id(), false),
            AccountMeta::new_readonly(chain_registration::derive_pda(&id, v.chain).0, false),
            AccountMeta::new_readonly(hub_key, false),
            AccountMeta::new_readonly(peer_src, false),
            AccountMeta::new_readonly(peer_dst, false),
        ]
    };
    send(
        &rpc,
        "submit_vaas[after cutover]",
        &submit_vaas_ix(
            "submit_vaas",
            settled,
            submit_tail(
                settled,
                source_balance,
                dest_balance,
                hub_pda,
                peer_src_pda,
                peer_dst_pda,
            ),
        ),
        &[&payer],
    );

    assert_bucket_marked(&read(&bucket(settled), "bucket after"), settled.sequence);
    let settled_amount = |debited: bool| if debited { Uint256::ZERO } else { amount };
    for (label, key, debited) in [
        ("source", source_balance, source_debited),
        ("destination", dest_balance, dest_debited),
    ] {
        let account = read(&key, "balance after");
        assert_eq!(account.owner, id, "{label} balance owner");
        assert_eq!(
            balance_of(&account),
            settled_amount(debited),
            "{label} balance after the transfer"
        );
    }
    assert_eq!(
        read(&hub_pda, "hub after"),
        hub_before,
        "the backfilled hub is read, not rewritten"
    );
    for (key, before) in &peers_before {
        assert_eq!(
            &read(key, "peer after"),
            before,
            "the backfilled peer {key} is read, not rewritten"
        );
    }

    // 4. A sequence wormchain already consumed stays consumed after the cutover.
    let consumed_recipient = consumed.expected_recipient_chain;
    send_expect_error(
        &rpc,
        "submit_vaas[backfilled sequence]",
        &submit_vaas_ix(
            "submit_vaas consumed",
            consumed,
            submit_tail(
                consumed,
                balance::derive_pda(&id, consumed.chain, hub.0, &hub.1).0,
                balance::derive_pda(&id, consumed_recipient, hub.0, &hub.1).0,
                pda::derive(
                    &id,
                    &TransceiverHubKey::new(consumed.chain, consumed.emitter),
                )
                .0,
                pda::derive(
                    &id,
                    &TransceiverPeerKey::new(consumed.chain, consumed.emitter, consumed_recipient),
                )
                .0,
                pda::derive(
                    &id,
                    &TransceiverPeerKey::new(consumed_recipient, CORPUS_PEER, consumed.chain),
                )
                .0,
            ),
        ),
        &[&payer],
        GlobalAccountantError::AlreadyAccounted,
    );
}
