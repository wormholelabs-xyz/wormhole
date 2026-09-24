use global_accountant::instructions::modify_balance::derive_modify_balance_pda;
use global_accountant::instructions::transfer::derive_balance_account_pda;
use global_accountant_definitions::{
    GlobalAccountantError, GovernanceHeader, ModificationKind, ModifyBalanceLayout, Uint256,
    ACCOUNTANT_GOVERNANCE_MODULE, GOVERNANCE_EMITTER, MODIFY_BALANCE_ACTION, SOLANA_CHAIN_ID,
};
use mollusk_svm::program::keyed_account_for_system_program;
use mollusk_svm::result::InstructionResult;
use mollusk_svm::Mollusk;
use solana_account::Account;
use solana_instruction::{AccountMeta, Instruction};
use solana_pubkey::Pubkey;

use crate::common::*;

const REASON: [u8; 32] = *b"audit-log: post-incident credit ";

fn solana_target() -> GovernanceHeader {
    governance_header(
        ACCOUNTANT_GOVERNANCE_MODULE,
        MODIFY_BALANCE_ACTION,
        SOLANA_CHAIN_ID,
    )
}

#[derive(Clone)]
struct Modification {
    payload_sequence: u64,
    kind: ModificationKind,
    amount: Uint256,
    body: Vec<u8>,
    guardian_set_bump: u8,
    payer: Pubkey,
    guardian_set: Pubkey,
    guardian_signatures: Pubkey,
    balance_pda: Pubkey,
    modify_balance_pda: Pubkey,
    guardians: Vec<Guardian>,
}

impl Modification {
    fn new(vaa_sequence: u64, payload_sequence: u64, kind: ModificationKind, amount: u128) -> Self {
        Self::with_header(
            solana_target(),
            vaa_sequence,
            payload_sequence,
            kind,
            amount,
        )
    }

    fn with_header(
        header: GovernanceHeader,
        vaa_sequence: u64,
        payload_sequence: u64,
        kind: ModificationKind,
        amount: u128,
    ) -> Self {
        let amount = Uint256::from_u128(amount);
        let body = modify_balance_body(
            SOLANA_CHAIN_ID,
            GOVERNANCE_EMITTER,
            vaa_sequence,
            header,
            payload_sequence,
            ETHEREUM,
            ETHEREUM,
            TOKEN_ADDRESS,
            kind as u8,
            amount,
            REASON,
        );
        let (guardian_set, guardian_set_bump) =
            derive_guardian_set_pda(GUARDIAN_SET_INDEX, &core_bridge_program_id());
        Self {
            payload_sequence,
            kind,
            amount,
            body,
            guardian_set_bump,
            payer: SUBMITTER,
            guardian_set,
            guardian_signatures: Pubkey::new_from_array([0xC3u8; 32]),
            balance_pda: derive_balance_account_pda(
                &program_id(),
                ETHEREUM,
                ETHEREUM,
                &TOKEN_ADDRESS,
            )
            .0,
            modify_balance_pda: derive_modify_balance_pda(&program_id(), payload_sequence).0,
            guardians: make_guardians(GUARDIAN_COUNT, 0x42),
        }
    }

    fn account_metas(&self) -> Vec<AccountMeta> {
        vec![
            AccountMeta::new(self.payer, true),
            AccountMeta::new_readonly(shim_program_id(), false),
            AccountMeta::new_readonly(self.guardian_set, false),
            AccountMeta::new_readonly(self.guardian_signatures, false),
            AccountMeta::new(self.balance_pda, false),
            AccountMeta::new_readonly(system_program_id(), false),
            AccountMeta::new(self.modify_balance_pda, false),
        ]
    }

    fn accounts(&self, balance: Account, record: Account) -> Vec<(Pubkey, Account)> {
        let digest = double_keccak256(&self.body);
        let signatures: Vec<(u8, [u8; 65])> = (0..QUORUM)
            .map(|i| (i, sign_digest(&self.guardians[i as usize], &digest)))
            .collect();
        let keys: Vec<[u8; GUARDIAN_PUBKEY_LENGTH]> =
            self.guardians.iter().map(|g| g.eth_address).collect();
        vec![
            (self.payer, system_owned_account(50_000_000_000)),
            keyed_account_for_verify_vaa_shim_program(),
            (
                self.guardian_set,
                guardian_set_account(GUARDIAN_SET_INDEX, &keys, 0, 0, &core_bridge_program_id()),
            ),
            (
                self.guardian_signatures,
                guardian_signatures_account(
                    GUARDIAN_SET_INDEX,
                    &self.payer,
                    &signatures,
                    &shim_program_id(),
                ),
            ),
            (self.balance_pda, balance),
            keyed_account_for_system_program(),
            (self.modify_balance_pda, record),
        ]
    }

    fn submit(&self, mollusk: &Mollusk, accounts: Vec<(Pubkey, Account)>) -> InstructionResult {
        let ix = Instruction::new_with_bytes(
            program_id(),
            &modify_balance_ix_data(self.guardian_set_bump, &self.body),
            self.account_metas(),
        );
        mollusk.process_instruction(&ix, &accounts)
    }

    fn expected_record(&self) -> ModifyBalanceLayout {
        ModifyBalanceLayout::new(
            self.kind,
            ETHEREUM,
            ETHEREUM,
            self.payload_sequence,
            TOKEN_ADDRESS,
            self.amount,
            REASON,
        )
    }
}

fn assert_record(accounts: &[(Pubkey, Account)], modification: &Modification) {
    let record = find_account(accounts, &modification.modify_balance_pda);
    assert_eq!(record.owner, program_id());
    assert_eq!(
        *bytemuck::from_bytes::<ModifyBalanceLayout>(&record.data),
        modification.expected_record()
    );
}

#[test]
fn add_creates_then_add_and_subtract_share_pda() {
    let mollusk = mollusk();

    let create = Modification::new(0x10, 200, ModificationKind::Add, 1_000_000);
    let r1 = create.submit(
        &mollusk,
        create.accounts(uninitialised_pda_account(), uninitialised_pda_account()),
    );
    assert_success(&r1, "create");
    assert_balance(
        &r1.resulting_accounts,
        &create.balance_pda,
        Uint256::from_u128(1_000_000),
    );
    assert_record(&r1.resulting_accounts, &create);
    let balance = find_account(&r1.resulting_accounts, &create.balance_pda).clone();

    let add = Modification::new(0x11, 201, ModificationKind::Add, 500);
    let r2 = add.submit(&mollusk, add.accounts(balance, uninitialised_pda_account()));
    assert_success(&r2, "add");
    assert_balance(
        &r2.resulting_accounts,
        &add.balance_pda,
        Uint256::from_u128(1_000_500),
    );
    assert_record(&r2.resulting_accounts, &add);
    let balance = find_account(&r2.resulting_accounts, &add.balance_pda).clone();

    let subtract = Modification::new(0x12, 202, ModificationKind::Subtract, 1_500);
    let r3 = subtract.submit(
        &mollusk,
        subtract.accounts(balance, uninitialised_pda_account()),
    );
    assert_success(&r3, "subtract");
    assert_balance(
        &r3.resulting_accounts,
        &subtract.balance_pda,
        Uint256::from_u128(999_000),
    );
    assert_record(&r3.resulting_accounts, &subtract);
    assert_ne!(add.modify_balance_pda, subtract.modify_balance_pda);
}

/// Regression test, PR 63 Bugbot HIGH finding: a `ModifyBalance` record PDA in the shape
/// the backfill program writes must block replay of the archived governance VAA, and the
/// balance — already carrying the historical delta, as the wormchain snapshot would — must
/// stay untouched.
#[test]
fn backfilled_record_blocks_replay_and_leaves_balance_unchanged() {
    let mollusk = mollusk();
    let modification = Modification::new(0x30, 400, ModificationKind::Add, 1_000_000);
    let backfilled_record = Account {
        lamports: 1_000_000,
        data: bytemuck::bytes_of(&modification.expected_record()).to_vec(),
        owner: program_id(),
        executable: false,
        rent_epoch: 0,
    };
    // Balance already includes this modification's effect.
    let balance = balance_account(ETHEREUM, ETHEREUM, TOKEN_ADDRESS, modification.amount);

    let accounts = modification.accounts(balance, backfilled_record);
    let before = accounts.clone();
    let result = modification.submit(&mollusk, accounts);

    assert_error(
        &result,
        GlobalAccountantError::DuplicateModifyBalance as u64,
        "backfilled record blocks replay",
    );
    assert_eq!(
        result.resulting_accounts, before,
        "balance and record must be untouched"
    );
}

#[test]
fn rejects() {
    let mollusk = mollusk();
    let mut wrong_module = solana_target();
    wrong_module.module[31] ^= 1;
    let existing_record = {
        let layout = Modification::new(0, 300, ModificationKind::Add, 1).expected_record();
        Account {
            lamports: 1_000_000,
            data: bytemuck::bytes_of(&layout).to_vec(),
            owner: program_id(),
            executable: false,
            rent_epoch: 0,
        }
    };
    let mut wrong_balance_pda = Modification::new(0x24, 304, ModificationKind::Add, 1);
    wrong_balance_pda.balance_pda = Pubkey::new_from_array([0xEEu8; 32]);

    let cases: [(&str, Modification, Account, Account, GlobalAccountantError); 5] = [
        (
            "wrong module",
            Modification::with_header(wrong_module, 0x20, 301, ModificationKind::Add, 1),
            uninitialised_pda_account(),
            uninitialised_pda_account(),
            GlobalAccountantError::InvalidGovernanceModule,
        ),
        (
            "subtract on uninitialised balance",
            Modification::new(0x21, 302, ModificationKind::Subtract, 1),
            uninitialised_pda_account(),
            uninitialised_pda_account(),
            GlobalAccountantError::ModifyBalanceUnderflow,
        ),
        (
            "add overflow",
            Modification::new(0x22, 303, ModificationKind::Add, 1),
            balance_account(ETHEREUM, ETHEREUM, TOKEN_ADDRESS, Uint256::MAX),
            uninitialised_pda_account(),
            GlobalAccountantError::ModifyBalanceOverflow,
        ),
        (
            "duplicate sequence",
            Modification::new(0x23, 300, ModificationKind::Add, 1),
            uninitialised_pda_account(),
            existing_record,
            GlobalAccountantError::DuplicateModifyBalance,
        ),
        (
            "wrong balance pda",
            wrong_balance_pda,
            uninitialised_pda_account(),
            uninitialised_pda_account(),
            GlobalAccountantError::InvalidPda,
        ),
    ];

    for (label, modification, balance, record, expected) in cases {
        let result = modification.submit(&mollusk, modification.accounts(balance, record));
        assert_error(&result, expected as u64, label);
    }
}
