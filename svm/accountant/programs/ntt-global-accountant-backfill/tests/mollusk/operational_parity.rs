//! Cross-`.so` check on the shared PDA path: a `TransceiverHub` PDA written by the backfill
//! `.so` is accepted by the NTT operational `.so` running at the same program id. The hub
//! account crosses from one binary to the other untouched, so `register_peer` reaches its
//! hub-match arm instead of `MissingTransceiverHub`.

use accountant_operational_core::cpi::noreplay::derive_bucket_pda;
use accountant_operational_core::support::pda;
use accountant_test_harness::mollusk_with_fixtures;
use global_accountant_definitions::ntt_global_accountant_backfill::Instruction;
use global_accountant_definitions::{
    TransceiverHubKey, TransceiverHubLayout, TransceiverPeerKey, TransceiverPeerLayout,
};
use mollusk_svm::program::keyed_account_for_system_program;
use solana_instruction::{AccountMeta, Instruction as SolanaInstruction};

use crate::common::*;
use crate::ntt_ix::{direct_body, peer_payload, register_peer_ix_data};

/// The NTT operational program's `.so` under `SBF_OUT_DIR`, at the shared program id.
const OPERATIONAL_PROGRAM_NAME: &str = "ntt_global_accountant";

const SOLANA: u16 = 1;
const ETHEREUM: u16 = 2;
const HUB: [u8; 32] = [0x7Bu8; 32];
const SPOKE: [u8; 32] = [0x7Au8; 32];
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
