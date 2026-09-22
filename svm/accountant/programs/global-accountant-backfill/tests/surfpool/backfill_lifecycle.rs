//! The backfill program's lifecycle against a fresh surfpool, with the pinned
//! `solana_noreplay.so` co-deployed:
//!
//! 1. `BackfillNoReplay`: three entries across two buckets. The bitmap bits flip and
//!    one `ACCDGST\0` record per entry reaches `meta.logMessages` over real RPC.
//! 2. `BackfillBalance`: two `BalanceAccountLayout` PDAs at the operational seeds.
//! 3. `BackfillChainRegistration`: two chains, each writing the `ChainRegistration`
//!    PDA and the `RegisterChain` record.
//! 4. `BackfillModifyBalance`: one `ModifyBalanceLayout` record.
//! 5. A stranger signs `BackfillNoReplay` and `BackfillBalance`: both reject with
//!    `UnauthorizedCaller`, and `getAccountInfo` on the target balance PDA errors.
//!
//! Every write is checked for owner, length, rent and layout, so the on-chain bytes
//! are the bytes the operational program reads after the cutover upgrade.

use accountant_operational_core::accounts::{balance, chain_registration};
use accountant_operational_core::cpi::noreplay::derive_bucket_pda;
use accountant_operational_core::instructions::modify_balance::derive_modify_balance_pda;
use accountant_operational_core::instructions::register_chain::derive_register_chain_pda;
use global_accountant_definitions::global_accountant_backfill::Instruction as Arm;
use global_accountant_definitions::{
    BalanceAccountLayout, ChainRegistrationLayout, GlobalAccountantError, ModificationKind,
    ModifyBalanceLayout, NoReplayBitmapAccount, RegisterChainLayout, Uint256,
    UNPINNED_GUARDIAN_SET_INDEX,
};
use solana_account::Account;
use solana_instruction::{AccountMeta, Instruction};
use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use solana_rent::Rent;
use solana_signer::Signer;

use crate::common::*;
use crate::harness::{
    accdgst_logs_in_tx, deploy_programs, fund, send, send_expect_error, start_surfpool,
    ProgramImage, SurfpoolOptions,
};

const BSC: u16 = 4;
const EMITTER: [u8; 32] = [0x42u8; 32];
const AUTHORITY_LAMPORTS: u64 = 20_000_000_000;
const STRANGER_LAMPORTS: u64 = 2_000_000_000;

/// Account written by the backfill: owner, length, rent and layout in one check.
fn assert_written<T: bytemuck::Pod + PartialEq + core::fmt::Debug>(
    account: &Account,
    program_id: &Pubkey,
    expected: T,
    label: &str,
) {
    let len = core::mem::size_of::<T>();
    assert_eq!(account.owner, *program_id, "{label}: owner");
    assert_eq!(account.data.len(), len, "{label}: data length");
    assert_eq!(
        account.lamports,
        Rent::default().minimum_balance(len),
        "{label}: rent-exempt minimum"
    );
    assert_eq!(layout::<T>(account), expected, "{label}: layout");
}

#[test]
#[ignore = "spawns surfpool subprocess; run via `just e2e-backfill`"]
fn surfpool_backfill_lifecycle() {
    let guard = start_surfpool(SurfpoolOptions::offline("ga-backfill-lifecycle"));
    let rpc = guard.rpc_client();

    let backfill = accountant_image();
    let id = backfill.program_id;
    deploy_programs(&rpc, &[backfill, ProgramImage::noreplay()]);

    // The payer is the compiled-in `BACKFILL_AUTHORITY`; every other signer is a stranger.
    let authority = test_authority_keypair();
    let stranger = Keypair::new();
    fund(&rpc, &authority.pubkey(), AUTHORITY_LAMPORTS);
    fund(&rpc, &stranger.pubkey(), STRANGER_LAMPORTS);

    let noreplay_authority = noreplay_authority_pda(&id);
    let bucket =
        |sequence: u64| derive_bucket_pda(&noreplay_authority, ETHEREUM, &EMITTER, sequence).0;
    let noreplay_head = |signer: &Pubkey| {
        vec![
            AccountMeta::new(*signer, true),
            AccountMeta::new_readonly(noreplay_program_id(), false),
            AccountMeta::new_readonly(noreplay_authority, false),
            AccountMeta::new_readonly(system_program_id(), false),
        ]
    };
    let write_head = |signer: &Pubkey| {
        vec![
            AccountMeta::new(*signer, true),
            AccountMeta::new_readonly(system_program_id(), false),
        ]
    };

    // 1. BackfillNoReplay: sequences 10 and 500 share bucket 0, 1500 opens bucket 1.
    let transfers = [
        wire::NoReplayEntry {
            chain: ETHEREUM,
            emitter: EMITTER,
            sequence: 10,
            digest: [0xa1u8; 32],
        },
        wire::NoReplayEntry {
            chain: ETHEREUM,
            emitter: EMITTER,
            sequence: 500,
            digest: [0xa2u8; 32],
        },
        wire::NoReplayEntry {
            chain: ETHEREUM,
            emitter: EMITTER,
            sequence: 1_500,
            digest: [0xa3u8; 32],
        },
    ];
    let mut accounts = noreplay_head(&authority.pubkey());
    accounts.push(AccountMeta::new(bucket(10), false));
    accounts.push(AccountMeta::new(bucket(1_500), false));
    let sig = send(
        &rpc,
        "backfill_no_replay",
        &[Instruction {
            program_id: id,
            accounts,
            data: wire::encode_noreplay_batch(Arm::BackfillNoReplay as u8, &transfers),
        }],
        &[&authority],
    );

    let logs = accdgst_logs_in_tx(&rpc, &sig);
    assert_eq!(
        logs.len(),
        transfers.len(),
        "one commit-log record per entry"
    );
    for (entry, log) in transfers.iter().zip(&logs) {
        let label = entry.sequence;
        assert_eq!(log.chain(), entry.chain, "commit-log chain {label}");
        assert_eq!(log.emitter, entry.emitter, "commit-log emitter {label}");
        assert_eq!(
            log.sequence(),
            entry.sequence,
            "commit-log sequence {label}"
        );
        assert_eq!(log.digest, entry.digest, "commit-log digest {label}");
        assert_eq!(
            log.guardian_set_index(),
            UNPINNED_GUARDIAN_SET_INDEX,
            "commit-log guardian_set_index {label}"
        );
    }
    for sequence in [10u64, 500, 1_500] {
        let account = rpc.get_account(&bucket(sequence)).expect("bucket PDA");
        assert_bucket_marked(&account, sequence);
    }
    assert_eq!(
        NoReplayBitmapAccount::bucket_index(1_500),
        1,
        "1500 belongs to the second bucket"
    );

    // 2. BackfillBalance.
    let balances = [
        wire::balance_entry(
            ETHEREUM,
            ETHEREUM,
            [0x11u8; 32],
            Uint256::from_u128(1_000_000).0,
        ),
        wire::balance_entry(BSC, BSC, [0x22u8; 32], Uint256::from_u128(2_500_000).0),
    ];
    let balance_pda = |entry: &global_accountant_definitions::BackfillBalanceEntry| {
        balance::derive_pda(
            &id,
            entry.chain(),
            entry.token_chain(),
            &entry.token_address,
        )
        .0
    };
    let mut accounts = write_head(&authority.pubkey());
    accounts.extend(
        balances
            .iter()
            .map(|entry| AccountMeta::new(balance_pda(entry), false)),
    );
    send(
        &rpc,
        "backfill_balance",
        &[Instruction {
            program_id: id,
            accounts,
            data: wire::encode_balance_batch(Arm::BackfillBalance as u8, &balances),
        }],
        &[&authority],
    );
    for entry in &balances {
        let account = rpc.get_account(&balance_pda(entry)).expect("balance PDA");
        assert_written(
            &account,
            &id,
            BalanceAccountLayout::new(
                entry.chain(),
                entry.token_chain(),
                entry.token_address,
                entry.balance(),
            ),
            "balance",
        );
    }

    // 3. BackfillChainRegistration: the registration PDA and the governance record.
    let registrations = [
        wire::chain_registration_entry(ETHEREUM, 1_234, [0x51u8; 32]),
        wire::chain_registration_entry(BSC, 77, [0x52u8; 32]),
    ];
    let mut accounts = write_head(&authority.pubkey());
    for entry in &registrations {
        accounts.push(AccountMeta::new(
            chain_registration::derive_pda(&id, entry.chain()).0,
            false,
        ));
        accounts.push(AccountMeta::new(
            derive_register_chain_pda(&id, entry.sequence()).0,
            false,
        ));
    }
    send(
        &rpc,
        "backfill_chain_registration",
        &[Instruction {
            program_id: id,
            accounts,
            data: wire::encode_chain_registration_batch(
                Arm::BackfillChainRegistration as u8,
                &registrations,
            ),
        }],
        &[&authority],
    );
    for entry in &registrations {
        let (chain, sequence, emitter) = (entry.chain(), entry.sequence(), entry.emitter);
        let account = rpc
            .get_account(&chain_registration::derive_pda(&id, chain).0)
            .expect("ChainRegistration PDA");
        assert_written(
            &account,
            &id,
            ChainRegistrationLayout::new(chain, emitter, sequence),
            "chain registration",
        );
        let account = rpc
            .get_account(&derive_register_chain_pda(&id, sequence).0)
            .expect("RegisterChain PDA");
        assert_written(
            &account,
            &id,
            RegisterChainLayout::new(chain, emitter, sequence),
            "register chain record",
        );
    }

    // 4. BackfillModifyBalance: the record that arms `modify_balance`'s replay guard.
    let reason: [u8; 32] = *b"wormchain modification sequence ";
    let modification = wire::modify_balance_entry(
        ModificationKind::Add as u8,
        ETHEREUM,
        ETHEREUM,
        9,
        [0x33u8; 32],
        Uint256::from_u128(4_200).0,
        reason,
    );
    let record = derive_modify_balance_pda(&id, modification.sequence()).0;
    let mut accounts = write_head(&authority.pubkey());
    accounts.push(AccountMeta::new(record, false));
    send(
        &rpc,
        "backfill_modify_balance",
        &[Instruction {
            program_id: id,
            accounts,
            data: wire::encode_modify_balance_batch(
                Arm::BackfillModifyBalance as u8,
                &[modification],
            ),
        }],
        &[&authority],
    );
    assert_written(
        &rpc.get_account(&record).expect("ModifyBalance PDA"),
        &id,
        ModifyBalanceLayout::new(
            ModificationKind::from_u8(modification.kind).expect("valid kind"),
            modification.chain_id(),
            modification.token_chain(),
            modification.sequence(),
            modification.token_address,
            modification.amount(),
            modification.reason,
        ),
        "modify balance record",
    );

    // 5. A stranger's writes: the compile-time authority gate runs in both handlers.
    let stranger_transfer = [wire::NoReplayEntry {
        chain: ETHEREUM,
        emitter: EMITTER,
        sequence: 9_999,
        digest: [0xb0u8; 32],
    }];
    let mut accounts = noreplay_head(&stranger.pubkey());
    accounts.push(AccountMeta::new(bucket(9_999), false));
    send_expect_error(
        &rpc,
        "backfill_no_replay[stranger]",
        &[Instruction {
            program_id: id,
            accounts,
            data: wire::encode_noreplay_batch(Arm::BackfillNoReplay as u8, &stranger_transfer),
        }],
        &[&stranger],
        GlobalAccountantError::UnauthorizedCaller,
    );

    let stranger_balance = wire::balance_entry(9, 9, [0x99u8; 32], Uint256::from_u128(1).0);
    let stranger_pda = balance_pda(&stranger_balance);
    let mut accounts = write_head(&stranger.pubkey());
    accounts.push(AccountMeta::new(stranger_pda, false));
    send_expect_error(
        &rpc,
        "backfill_balance[stranger]",
        &[Instruction {
            program_id: id,
            accounts,
            data: wire::encode_balance_batch(Arm::BackfillBalance as u8, &[stranger_balance]),
        }],
        &[&stranger],
        GlobalAccountantError::UnauthorizedCaller,
    );
    assert!(
        rpc.get_account(&stranger_pda).is_err(),
        "rejected BackfillBalance wrote its target PDA"
    );
}
