//! The NTT accountant lifecycle against the real Verify VAA Shim and NoReplay programs on a
//! fresh surfpool, with synthetic VAAs signed by the test guardian set:
//!
//! 1. `register_hub`: the Solana hub registers itself.
//! 2. `register_peer`: the hub pre-registers its Ethereum spoke; the spoke adopts the hub.
//! 3. `register_relayer_chain`: governance names Ethereum's Standard Relayer.
//! 4. `submit_vaas`: a relayed spoke-to-hub transfer burns wrapped and unlocks native.
//! 5. `submit_observations`: thirteen guardians commit a hub-to-spoke transfer; a fourteenth
//!    observation and a `submit_vaas` replay both reject with `AlreadyAccounted`.
//! 6. `modify_balance`: governance credits the spoke balance; replaying the payload sequence
//!    rejects with `DuplicateModifyBalance`.

use accountant_operational_core::accounts::{balance, chain_registration};
use accountant_operational_core::cpi::noreplay::derive_bucket_pda;
use accountant_operational_core::instructions::modify_balance::derive_modify_balance_pda;
use accountant_operational_core::instructions::register_chain::derive_register_chain_pda;
use accountant_operational_core::support::pda;
use global_accountant_definitions::{
    ChainRegistrationLayout, GlobalAccountantError, ManagerMode, ModificationKind,
    TransceiverHubLayout, TransceiverKey, TransceiverPeerKey, TransceiverPeerLayout, Uint256,
    GOVERNANCE_EMITTER, MODIFY_BALANCE_ACTION, NTT_ACCOUNTANT_GOVERNANCE_MODULE,
    REGISTER_CHAIN_ACTION, RELAYER_GOVERNANCE_MODULE, SOLANA_CHAIN_ID,
};
use solana_instruction::{AccountMeta, Instruction};
use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use solana_signer::Signer;

use crate::common::*;
use crate::harness::{
    assert_canonical_log_in_tx, deploy_guardian_set, deploy_programs, fund, send,
    send_expect_error, set_account, start_surfpool, ProgramImage, SurfpoolOptions,
};

const PAYER_LAMPORTS: u64 = 20_000_000_000;
/// The shim's `verify_vaa` CPI alone consumes ~196k CU with a full quorum.
const SHIM_CU_LIMIT: u32 = 400_000;
const SOLANA_HUB: (u16, [u8; 32]) = (SOLANA, HUB);
/// 1.5 tokens at 6 decimals; the accountant books it at 8.
const DECIMALS: u8 = 6;
const AMOUNT: u64 = 1_500_000;
const BOOKED: u128 = 150_000_000;
const CREDIT: u128 = 500;
const REASON: [u8; 32] = *b"audit-log: post-incident credit ";

#[test]
#[ignore = "spawns surfpool subprocess; run via `just e2e`"]
fn surfpool_ntt_lifecycle() {
    let guard = start_surfpool(SurfpoolOptions::offline("ntt-surfpool-lifecycle"));
    let rpc = guard.rpc_client();

    let accountant = accountant_image();
    let id = accountant.program_id;
    deploy_programs(
        &rpc,
        &[
            accountant,
            ProgramImage::verify_vaa_shim(),
            ProgramImage::noreplay(),
        ],
    );

    let payer = Keypair::new();
    fund(&rpc, &payer.pubkey(), PAYER_LAMPORTS);

    let guardians = make_guardians(GUARDIAN_COUNT, 0x42);
    let (guardian_set, guardian_set_bump) =
        deploy_guardian_set(&rpc, GUARDIAN_SET_INDEX, &guardians);

    // Sign `body` with the test guardians, post the signatures through the real shim, and
    // return the four Shim-facing account metas every VAA instruction starts with.
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
    let with_budget = |ix: Instruction| [set_compute_unit_limit_ix(SHIM_CU_LIMIT), ix];

    let noreplay_authority = noreplay_authority_pda(&id);
    let noreplay_tail = |bucket: Pubkey| {
        vec![
            AccountMeta::new(bucket, false),
            AccountMeta::new_readonly(noreplay_program_id(), false),
            AccountMeta::new_readonly(noreplay_authority, false),
            AccountMeta::new_readonly(system_program_id(), false),
        ]
    };
    let relayer_registration_pda = chain_registration::derive_pda(&id, ETHEREUM).0;
    let hub_key = TransceiverKey::new(SOLANA, HUB);
    let spoke_key = TransceiverKey::new(ETHEREUM, SPOKE);
    let hub_pda = pda::derive(&id, &hub_key).0;
    let spoke_hub_pda = pda::derive(&id, &spoke_key).0;
    let hub_peer_pda = pda::derive(&id, &TransceiverPeerKey::new(SOLANA, HUB, ETHEREUM)).0;
    let spoke_peer_pda = pda::derive(&id, &TransceiverPeerKey::new(ETHEREUM, SPOKE, SOLANA)).0;

    // 1. register_hub.
    let body = direct_body(SOLANA, HUB, 1, &hub_payload(ManagerMode::Locking));
    let mut accounts = shim_metas("register_hub", &body);
    accounts.push(AccountMeta::new_readonly(
        chain_registration::derive_pda(&id, SOLANA).0,
        false,
    ));
    accounts.push(AccountMeta::new(hub_pda, false));
    accounts.extend(noreplay_tail(
        derive_bucket_pda(&noreplay_authority, SOLANA, &HUB, 1).0,
    ));
    send(
        &rpc,
        "register_hub",
        &with_budget(Instruction {
            program_id: id,
            accounts,
            data: register_hub_ix_data(guardian_set_bump, &body),
        }),
        &[&payer],
    );
    assert_eq!(
        layout::<TransceiverHubLayout>(&rpc.get_account(&hub_pda).expect("hub PDA")),
        TransceiverHubLayout::new(hub_key, hub_key),
        "self-referential hub"
    );

    // 2. register_peer, both directions.
    let register_peer = |label: &str,
                         sequence: u64,
                         (chain, sender): (u16, [u8; 32]),
                         (dest_chain, peer): (u16, [u8; 32])| {
        let body = direct_body(chain, sender, sequence, &peer_payload(dest_chain, peer));
        let mut accounts = shim_metas(label, &body);
        accounts.extend([
            AccountMeta::new_readonly(chain_registration::derive_pda(&id, chain).0, false),
            AccountMeta::new(
                pda::derive(&id, &TransceiverKey::new(chain, sender)).0,
                false,
            ),
            AccountMeta::new_readonly(
                pda::derive(&id, &TransceiverKey::new(dest_chain, peer)).0,
                false,
            ),
            AccountMeta::new_readonly(
                pda::derive(&id, &TransceiverPeerKey::new(dest_chain, peer, chain)).0,
                false,
            ),
            AccountMeta::new(
                pda::derive(&id, &TransceiverPeerKey::new(chain, sender, dest_chain)).0,
                false,
            ),
        ]);
        accounts.extend(noreplay_tail(
            derive_bucket_pda(&noreplay_authority, chain, &sender, sequence).0,
        ));
        send(
            &rpc,
            label,
            &with_budget(Instruction {
                program_id: id,
                accounts,
                data: register_peer_ix_data(guardian_set_bump, &body),
            }),
            &[&payer],
        );
    };
    register_peer(
        "register_peer[hub -> spoke]",
        2,
        (SOLANA, HUB),
        (ETHEREUM, SPOKE),
    );
    assert_eq!(
        layout::<TransceiverPeerLayout>(&rpc.get_account(&hub_peer_pda).expect("hub peer PDA")),
        peer_layout(SOLANA, HUB, ETHEREUM, SPOKE)
    );
    register_peer(
        "register_peer[spoke adopts hub]",
        1,
        (ETHEREUM, SPOKE),
        (SOLANA, HUB),
    );
    assert_eq!(
        layout::<TransceiverHubLayout>(&rpc.get_account(&spoke_hub_pda).expect("spoke hub PDA")),
        TransceiverHubLayout::new(spoke_key, hub_key),
        "spoke adopted the hub"
    );
    assert_eq!(
        layout::<TransceiverPeerLayout>(&rpc.get_account(&spoke_peer_pda).expect("spoke peer PDA")),
        peer_layout(ETHEREUM, SPOKE, SOLANA, HUB)
    );

    // 3. register_relayer_chain.
    let body = register_chain_body(
        SOLANA_CHAIN_ID,
        GOVERNANCE_EMITTER,
        10,
        governance_header(RELAYER_GOVERNANCE_MODULE, REGISTER_CHAIN_ACTION, 0),
        ETHEREUM,
        RELAYER,
    );
    let mut accounts = shim_metas("register_relayer_chain", &body);
    accounts.extend([
        AccountMeta::new(relayer_registration_pda, false),
        AccountMeta::new_readonly(system_program_id(), false),
        AccountMeta::new(derive_register_chain_pda(&id, 10).0, false),
    ]);
    send(
        &rpc,
        "register_relayer_chain",
        &with_budget(Instruction {
            program_id: id,
            accounts,
            data: register_relayer_chain_ix_data(guardian_set_bump, &body),
        }),
        &[&payer],
    );
    assert_eq!(
        layout::<ChainRegistrationLayout>(
            &rpc.get_account(&relayer_registration_pda)
                .expect("relayer registration")
        ),
        ChainRegistrationLayout::new(ETHEREUM, RELAYER, 10)
    );

    // 4. submit_vaas: relayed spoke -> hub. Wrapped source burns, native destination unlocks.
    let spoke_balance = balance::derive_pda(&id, ETHEREUM, SOLANA, &HUB).0;
    let hub_balance = balance::derive_pda(&id, SOLANA, SOLANA, &HUB).0;
    set_account(
        &rpc,
        &spoke_balance,
        &balance_account(ETHEREUM, SOLANA, HUB, Uint256::from_u128(BOOKED)),
    );
    set_account(
        &rpc,
        &hub_balance,
        &balance_account(SOLANA, SOLANA, HUB, Uint256::from_u128(BOOKED)),
    );
    let transfer_body = relayed_body(
        ETHEREUM,
        RELAYER,
        7,
        SPOKE,
        &transfer_payload(DECIMALS, AMOUNT, SOLANA),
    );
    let transfer_digest = double_keccak256(&transfer_body);
    let transfer_bucket = derive_bucket_pda(&noreplay_authority, ETHEREUM, &RELAYER, 7).0;
    let submit_vaas_ix = |label: &str| -> Instruction {
        let mut accounts = shim_metas(label, &transfer_body);
        accounts.extend([
            AccountMeta::new(transfer_bucket, false),
            AccountMeta::new_readonly(noreplay_program_id(), false),
            AccountMeta::new_readonly(noreplay_authority, false),
            AccountMeta::new(spoke_balance, false),
            AccountMeta::new(hub_balance, false),
            AccountMeta::new_readonly(system_program_id(), false),
            AccountMeta::new_readonly(relayer_registration_pda, false),
            AccountMeta::new_readonly(spoke_hub_pda, false),
            AccountMeta::new_readonly(spoke_peer_pda, false),
            AccountMeta::new_readonly(hub_peer_pda, false),
        ]);
        Instruction {
            program_id: id,
            accounts,
            data: submit_vaas_ix_data(guardian_set_bump, &transfer_body),
        }
    };
    let sig = send(
        &rpc,
        "submit_vaas[relayed spoke -> hub]",
        &with_budget(submit_vaas_ix("submit_vaas")),
        &[&payer],
    );
    assert_canonical_log_in_tx(
        &rpc,
        &sig,
        ETHEREUM,
        &RELAYER,
        7,
        &transfer_digest,
        GUARDIAN_SET_INDEX,
    );
    assert_bucket_marked(&rpc.get_account(&transfer_bucket).expect("bucket"), 7);
    for (label, key) in [("spoke", spoke_balance), ("hub", hub_balance)] {
        let account = rpc.get_account(&key).expect("balance PDA");
        assert_eq!(account.owner, id, "{label} balance owner");
        assert_eq!(
            balance_of(&account),
            Uint256::ZERO,
            "{label} balance after burn"
        );
    }
    send_expect_error(
        &rpc,
        "submit_vaas[replay]",
        &with_budget(submit_vaas_ix("submit_vaas replay")),
        &[&payer],
        GlobalAccountantError::AlreadyAccounted,
    );

    // 5. submit_observations: hub -> spoke at quorum. Native source locks, wrapped spoke mints.
    let obs = ObsScenario::new(
        Observation::direct(SOLANA, HUB, 3, ETHEREUM, DECIMALS, AMOUNT),
        SPOKE,
        SOLANA_HUB,
    );
    let metas = obs.account_metas_for(payer.pubkey());
    let mut quorum_sig = None;
    for index in 0..QUORUM {
        quorum_sig = Some(send(
            &rpc,
            &format!("submit_observations[{index}]"),
            &[Instruction {
                program_id: id,
                accounts: metas.clone(),
                data: obs.ix_data(index),
            }],
            &[&payer],
        ));
    }
    let quorum_sig = quorum_sig.expect("QUORUM > 0");
    assert_canonical_log_in_tx(
        &rpc,
        &quorum_sig,
        SOLANA,
        &HUB,
        3,
        &obs.obs.content_digest(),
        GUARDIAN_SET_INDEX,
    );
    assert_bucket_marked(&rpc.get_account(&obs.noreplay_bucket).expect("bucket"), 3);
    assert!(
        rpc.get_account(&obs.pending_pda).is_err(),
        "pending PDA closed"
    );
    for (label, key) in [("hub", hub_balance), ("spoke", spoke_balance)] {
        assert_eq!(
            balance_of(&rpc.get_account(&key).expect("balance PDA")),
            Uint256::from_u128(BOOKED),
            "{label} balance after quorum"
        );
    }
    send_expect_error(
        &rpc,
        "submit_observations[after quorum]",
        &[Instruction {
            program_id: id,
            accounts: metas.clone(),
            data: obs.ix_data(QUORUM),
        }],
        &[&payer],
        GlobalAccountantError::AlreadyAccounted,
    );

    // 6. modify_balance: credit the spoke, then replay the payload sequence.
    let modify_ix = |label: &str| -> Instruction {
        let body = modify_balance_body(
            SOLANA_CHAIN_ID,
            GOVERNANCE_EMITTER,
            20,
            governance_header(
                NTT_ACCOUNTANT_GOVERNANCE_MODULE,
                MODIFY_BALANCE_ACTION,
                SOLANA_CHAIN_ID,
            ),
            200,
            ETHEREUM,
            SOLANA,
            HUB,
            ModificationKind::Add as u8,
            Uint256::from_u128(CREDIT),
            REASON,
        );
        let mut accounts = shim_metas(label, &body);
        accounts.extend([
            AccountMeta::new(spoke_balance, false),
            AccountMeta::new_readonly(system_program_id(), false),
            AccountMeta::new(derive_modify_balance_pda(&id, 200).0, false),
        ]);
        Instruction {
            program_id: id,
            accounts,
            data: modify_balance_ix_data(guardian_set_bump, &body),
        }
    };
    send(
        &rpc,
        "modify_balance[add]",
        &with_budget(modify_ix("modify_balance")),
        &[&payer],
    );
    assert_eq!(
        balance_of(&rpc.get_account(&spoke_balance).expect("spoke balance")),
        Uint256::from_u128(BOOKED + CREDIT),
        "spoke balance after credit"
    );
    send_expect_error(
        &rpc,
        "modify_balance[replay]",
        &with_budget(modify_ix("modify_balance replay")),
        &[&payer],
        GlobalAccountantError::DuplicateModifyBalance,
    );
}
