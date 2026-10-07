//! Every shared handler, driven through the NTT program's own entrypoint: each write lands
//! under the NTT program id with the account tag the NTT operational program reads, and
//! `BackfillNoReplay` still flips the bucket bit and emits its `ACCDGST\0` commit log.

use accountant_operational_core::accounts::{balance, chain_registration};
use accountant_operational_core::cpi::noreplay::derive_bucket_pda;
use accountant_operational_core::instructions::modify_balance::derive_modify_balance_pda;
use accountant_operational_core::instructions::register_chain::derive_register_chain_pda;
use global_accountant_definitions::ntt_global_accountant_backfill::Instruction;
use global_accountant_definitions::{
    BalanceAccountLayout, ChainRegistrationLayout, ModificationKind, ModifyBalanceLayout,
    RegisterChainLayout, Uint256, ACCOUNTANT_DIGEST_LOG_TAG,
};
use mollusk_svm::program::keyed_account_for_system_program;
use solana_account::Account;
use solana_instruction::{AccountMeta, Instruction as SolanaInstruction};
use solana_pubkey::Pubkey;
use solana_svm_log_collector::LogCollector;

use crate::common::wire::NoReplayEntry;
use crate::common::*;

const EMITTER: [u8; 32] = [0x11u8; 32];
const TOKEN_ADDRESS: [u8; 32] = [0x22u8; 32];
const SEQUENCE: u64 = 42;

/// One instruction: wire bytes, the accounts behind them, and what the NTT program must
/// leave behind. `written` is the PDAs the program itself creates, each with the
/// `AccountTag` the operational program expects at offset 0.
struct Case {
    label: &'static str,
    data: Vec<u8>,
    accounts: Vec<(Pubkey, Account)>,
    metas: Vec<AccountMeta>,
    written: Vec<(Pubkey, u8)>,
    marked_bucket: Option<(Pubkey, u64)>,
    accdgst_logs: usize,
}

fn payer_account(signer: Pubkey) -> (Pubkey, Account) {
    (signer, system_owned_account(10_000_000_000))
}

/// Accounts and metas for a handler that takes `[payer, system_program, pdas @ ..]`.
fn simple_invocation(
    signer: Pubkey,
    pdas: &[Pubkey],
) -> (Vec<(Pubkey, Account)>, Vec<AccountMeta>) {
    let mut accounts = vec![payer_account(signer), keyed_account_for_system_program()];
    let mut metas = vec![
        AccountMeta::new(signer, true),
        AccountMeta::new_readonly(system_program_id(), false),
    ];
    for pda in pdas {
        accounts.push((*pda, uninitialised_pda_account()));
        metas.push(AccountMeta::new(*pda, false));
    }
    (accounts, metas)
}

fn noreplay_case(signer: Pubkey) -> Case {
    let entries = [NoReplayEntry {
        chain: ETHEREUM,
        emitter: EMITTER,
        sequence: SEQUENCE,
        digest: [0x77u8; 32],
    }];
    let authority = noreplay_authority_pda(&program_id());
    let bucket = derive_bucket_pda(&authority, ETHEREUM, &EMITTER, SEQUENCE).0;

    let accounts = vec![
        payer_account(signer),
        keyed_account_for_noreplay_program(),
        (authority, uninitialised_pda_account()),
        keyed_account_for_system_program(),
        (bucket, noreplay_bucket_unmarked()),
    ];
    let metas = vec![
        AccountMeta::new(signer, true),
        AccountMeta::new_readonly(noreplay_program_id(), false),
        AccountMeta::new_readonly(authority, false),
        AccountMeta::new_readonly(system_program_id(), false),
        AccountMeta::new(bucket, false),
    ];
    Case {
        label: "BackfillNoReplay",
        data: wire::encode_noreplay_batch(Instruction::BackfillNoReplay as u8, &entries),
        accounts,
        metas,
        written: Vec::new(),
        marked_bucket: Some((bucket, SEQUENCE)),
        accdgst_logs: 1,
    }
}

fn balance_case(signer: Pubkey) -> Case {
    let entry = wire::balance_entry(ETHEREUM, ETHEREUM, TOKEN_ADDRESS, Uint256::from_u128(7).0);
    let pda = balance::derive_pda(&program_id(), ETHEREUM, ETHEREUM, &TOKEN_ADDRESS).0;
    let (accounts, metas) = simple_invocation(signer, &[pda]);
    Case {
        label: "BackfillBalance",
        data: wire::encode_balance_batch(Instruction::BackfillBalance as u8, &[entry]),
        accounts,
        metas,
        written: vec![(pda, BalanceAccountLayout::TAG)],
        marked_bucket: None,
        accdgst_logs: 0,
    }
}

fn modify_balance_case(signer: Pubkey) -> Case {
    let entry = wire::modify_balance_entry(
        ModificationKind::Add as u8,
        ETHEREUM,
        ETHEREUM,
        SEQUENCE,
        TOKEN_ADDRESS,
        Uint256::from_u128(9).0,
        [0x01u8; 32],
    );
    let pda = derive_modify_balance_pda(&program_id(), SEQUENCE).0;
    let (accounts, metas) = simple_invocation(signer, &[pda]);
    Case {
        label: "BackfillModifyBalance",
        data: wire::encode_modify_balance_batch(Instruction::BackfillModifyBalance as u8, &[entry]),
        accounts,
        metas,
        written: vec![(pda, ModifyBalanceLayout::TAG)],
        marked_bucket: None,
        accdgst_logs: 0,
    }
}

fn relayer_chain_registration_case(signer: Pubkey) -> Case {
    let entry = wire::chain_registration_entry(ETHEREUM, SEQUENCE, EMITTER);
    let registration = chain_registration::derive_pda(&program_id(), ETHEREUM).0;
    let record = derive_register_chain_pda(&program_id(), SEQUENCE).0;
    let (accounts, metas) = simple_invocation(signer, &[registration, record]);
    Case {
        label: "BackfillRelayerChainRegistration",
        data: wire::encode_chain_registration_batch(
            Instruction::BackfillRelayerChainRegistration as u8,
            &[entry],
        ),
        accounts,
        metas,
        written: vec![
            (registration, ChainRegistrationLayout::TAG),
            (record, RegisterChainLayout::TAG),
        ],
        marked_bucket: None,
        accdgst_logs: 0,
    }
}

/// `Program data: <base64>` lines whose payload carries the accountant commit-log tag.
fn accdgst_log_count(messages: &[String]) -> usize {
    use base64::Engine;

    messages
        .iter()
        .filter_map(|line| line.strip_prefix("Program data: "))
        .filter_map(|b64| {
            base64::engine::general_purpose::STANDARD
                .decode(b64.trim())
                .ok()
        })
        .filter(|bytes| bytes.len() >= 8 && bytes[..8] == ACCOUNTANT_DIGEST_LOG_TAG)
        .count()
}

#[test]
fn every_shared_handler_writes_through_the_ntt_program_id() {
    let signer = ntt_test_authority_pubkey();
    let cases: [Case; 4] = [
        noreplay_case(signer),
        balance_case(signer),
        modify_balance_case(signer),
        relayer_chain_registration_case(signer),
    ];

    let mut mollusk = mollusk();
    for case in cases {
        let logs = LogCollector::new_ref();
        mollusk.logger = Some(logs.clone());

        let ix = SolanaInstruction::new_with_bytes(program_id(), &case.data, case.metas);
        let result = mollusk.process_instruction(&ix, &case.accounts);
        assert_success(&result, case.label);

        for (pda, tag) in &case.written {
            let account = find_account(&result.resulting_accounts, pda);
            assert_eq!(account.owner, program_id(), "{}: owner {pda}", case.label);
            assert_eq!(account.data[0], *tag, "{}: tag {pda}", case.label);
        }
        if let Some((bucket, sequence)) = case.marked_bucket {
            assert_bucket_marked(find_account(&result.resulting_accounts, &bucket), sequence);
        }
        assert_eq!(
            accdgst_log_count(logs.borrow().get_recorded_content()),
            case.accdgst_logs,
            "{}: ACCDGST records",
            case.label
        );
    }
}
