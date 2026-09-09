use accountant_operational_core::accounts::chain_registration;
use accountant_operational_core::cpi::noreplay::derive_bucket_pda;
use global_accountant_definitions::{
    ChainRegistrationLayout, GlobalAccountantError, GovernanceHeader, Uint256, GOVERNANCE_EMITTER,
    NOREPLAY_AUTHORITY_SEED_PREFIX, REGISTER_CHAIN_ACTION, SOLANA_CHAIN_ID,
    TOKEN_BRIDGE_GOVERNANCE_MODULE,
};
use mollusk_svm::program::keyed_account_for_system_program;
use mollusk_svm::result::InstructionResult;
use mollusk_svm::Mollusk;
use solana_account::Account;
use solana_instruction::{AccountMeta, Instruction};
use solana_pubkey::Pubkey;

use crate::common::*;

const WORMCHAIN: u16 = 3104;

fn any_target() -> GovernanceHeader {
    governance_header(TOKEN_BRIDGE_GOVERNANCE_MODULE, REGISTER_CHAIN_ACTION, 0)
}

#[derive(Clone)]
struct Registration {
    sequence: u64,
    body: Vec<u8>,
    guardian_set_bump: u8,
    payer: Pubkey,
    guardian_set: Pubkey,
    guardian_signatures: Pubkey,
    registration_pda: Pubkey,
    noreplay_bucket: Pubkey,
    noreplay_authority: Pubkey,
    guardians: Vec<Guardian>,
}

impl Registration {
    fn new(sequence: u64, chain: u16, emitter: [u8; 32]) -> Self {
        Self::with_body(
            sequence,
            chain,
            register_chain_body(
                SOLANA_CHAIN_ID,
                GOVERNANCE_EMITTER,
                sequence,
                any_target(),
                chain,
                emitter,
            ),
        )
    }

    fn with_body(sequence: u64, chain: u16, body: Vec<u8>) -> Self {
        let (noreplay_authority, _) =
            Pubkey::find_program_address(&[NOREPLAY_AUTHORITY_SEED_PREFIX], &program_id());
        let (guardian_set, guardian_set_bump) =
            derive_guardian_set_pda(GUARDIAN_SET_INDEX, &core_bridge_program_id());
        Self {
            sequence,
            body,
            guardian_set_bump,
            payer: SUBMITTER,
            guardian_set,
            guardian_signatures: GUARDIAN_SIGNATURES,
            registration_pda: chain_registration::derive_pda(&program_id(), chain).0,
            noreplay_bucket: derive_bucket_pda(
                &noreplay_authority,
                SOLANA_CHAIN_ID,
                &GOVERNANCE_EMITTER,
                sequence,
            )
            .0,
            noreplay_authority,
            guardians: make_guardians(GUARDIAN_COUNT, 0x42),
        }
    }

    fn account_metas(&self) -> Vec<AccountMeta> {
        vec![
            AccountMeta::new(self.payer, true),
            AccountMeta::new_readonly(shim_program_id(), false),
            AccountMeta::new_readonly(self.guardian_set, false),
            AccountMeta::new_readonly(self.guardian_signatures, false),
            AccountMeta::new(self.registration_pda, false),
            AccountMeta::new(self.noreplay_bucket, false),
            AccountMeta::new_readonly(noreplay_program_id(), false),
            AccountMeta::new_readonly(self.noreplay_authority, false),
            AccountMeta::new_readonly(system_program_id(), false),
        ]
    }

    fn accounts(&self, registration: Account, bucket: Account) -> Vec<(Pubkey, Account)> {
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
            (self.registration_pda, registration),
            (self.noreplay_bucket, bucket),
            keyed_account_for_noreplay_program(),
            (self.noreplay_authority, system_owned_account(0)),
            keyed_account_for_system_program(),
        ]
    }

    fn submit(&self, mollusk: &Mollusk, accounts: Vec<(Pubkey, Account)>) -> InstructionResult {
        let ix = Instruction::new_with_bytes(
            program_id(),
            &register_chain_ix_data(self.guardian_set_bump, &self.body),
            self.account_metas(),
        );
        mollusk.process_instruction(&ix, &accounts)
    }
}

fn registration_layout(account: &Account) -> ChainRegistrationLayout {
    *bytemuck::from_bytes::<ChainRegistrationLayout>(&account.data)
}

#[test]
fn register_rotate_and_submit_vaas_flow() {
    let mollusk = mollusk();
    let emitter_a = [0x77u8; 32];
    let emitter_b = [0xBBu8; 32];

    let first = Registration::new(6, ETHEREUM, emitter_a);
    let r1 = first.submit(
        &mollusk,
        first.accounts(uninitialised_pda_account(), noreplay_bucket_unmarked()),
    );
    assert_success(&r1, "first registration");
    let registration = find_account(&r1.resulting_accounts, &first.registration_pda);
    assert_eq!(registration.owner, program_id());
    assert_eq!(
        registration_layout(registration),
        ChainRegistrationLayout::new(ETHEREUM, emitter_a, 6)
    );
    assert_bucket_marked(
        find_account(&r1.resulting_accounts, &first.noreplay_bucket),
        first.sequence,
    );

    let rotation = Registration::new(7, ETHEREUM, emitter_b);
    let r2 = rotation.submit(
        &mollusk,
        rotation.accounts(registration.clone(), noreplay_bucket_unmarked()),
    );
    assert_success(&r2, "rotation");
    let rotated = find_account(&r2.resulting_accounts, &rotation.registration_pda);
    assert_eq!(
        registration_layout(rotated),
        ChainRegistrationLayout::new(ETHEREUM, emitter_b, 7)
    );

    let mut transfer = Transfer::new(0, ETHEREUM, SOLANA_CHAIN_ID, 500_000);
    transfer.emitter = emitter_b;
    let vaas = VaaScenario::transfer(transfer);
    let mut accounts = vaas.accounts();
    replace_account(&mut accounts, &vaas.chain_registration, rotated.clone());
    let r3 = vaas.submit(&mollusk, accounts);
    assert_success(&r3, "submit_vaas from rotated emitter");
    assert_balance(
        &r3.resulting_accounts,
        &vaas.dest_account,
        Uint256::from_u128(500_000),
    );
}

/// A genuine older `RegisterChain` VAA, never applied here, must not roll the emitter back
/// after a newer one has been applied. NoReplay cannot catch it; only the stored sequence can.
#[test]
fn older_registration_after_newer_is_stale() {
    let mollusk = mollusk();
    let emitter_a = [0x77u8; 32];
    let emitter_b = [0xBBu8; 32];

    let newer = Registration::new(7, ETHEREUM, emitter_b);
    let applied = newer.submit(
        &mollusk,
        newer.accounts(uninitialised_pda_account(), noreplay_bucket_unmarked()),
    );
    assert_success(&applied, "newer registration");
    let registered = find_account(&applied.resulting_accounts, &newer.registration_pda).clone();

    let older = Registration::new(5, ETHEREUM, emitter_a);
    let accounts = older.accounts(registered.clone(), noreplay_bucket_unmarked());
    let result = older.submit(&mollusk, accounts);
    assert_error(
        &result,
        GlobalAccountantError::StaleRegistration as u64,
        "older registration",
    );
    assert_eq!(
        find_account(&result.resulting_accounts, &older.registration_pda),
        &registered,
        "registration unchanged"
    );
}

#[test]
fn rejects() {
    let mollusk = mollusk();
    let emitter_a = [0x77u8; 32];
    let emitter_b = [0xBBu8; 32];
    let registered_b = chain_registration_account(ETHEREUM, emitter_b);

    let body = |header: GovernanceHeader, emitter_chain: u16, sequence: u64| {
        register_chain_body(
            emitter_chain,
            GOVERNANCE_EMITTER,
            sequence,
            header,
            ETHEREUM,
            emitter_a,
        )
    };
    let mut wrong_module = any_target();
    wrong_module.module[31] ^= 1;
    let mut wrong_action = any_target();
    wrong_action.action = 2;

    let mut spoofed_pda = Registration::new(15, ETHEREUM, emitter_a);
    spoofed_pda.registration_pda = chain_registration::derive_pda(&program_id(), 3).0;

    let cases: [(&str, Registration, Account, Account, GlobalAccountantError); 5] = [
        (
            "wrong module",
            Registration::with_body(10, ETHEREUM, body(wrong_module, SOLANA_CHAIN_ID, 10)),
            uninitialised_pda_account(),
            noreplay_bucket_unmarked(),
            GlobalAccountantError::InvalidGovernanceModule,
        ),
        (
            "wrong action",
            Registration::with_body(11, ETHEREUM, body(wrong_action, SOLANA_CHAIN_ID, 11)),
            uninitialised_pda_account(),
            noreplay_bucket_unmarked(),
            GlobalAccountantError::InvalidGovernanceAction,
        ),
        (
            "wrong governance emitter chain",
            Registration::with_body(12, ETHEREUM, body(any_target(), ETHEREUM, 12)),
            uninitialised_pda_account(),
            noreplay_bucket_unmarked(),
            GlobalAccountantError::InvalidGovernanceEmitter,
        ),
        (
            "old vaa replayed after rotation",
            Registration::new(6, ETHEREUM, emitter_a),
            registered_b,
            noreplay_bucket_marked(6),
            GlobalAccountantError::AlreadyAccounted,
        ),
        (
            "non-canonical registration pda",
            spoofed_pda,
            uninitialised_pda_account(),
            noreplay_bucket_unmarked(),
            GlobalAccountantError::InvalidPda,
        ),
    ];

    for (label, registration, pda_state, bucket_state, expected) in cases {
        let result = registration.submit(&mollusk, registration.accounts(pda_state, bucket_state));
        assert_error(&result, expected as u64, label);
    }
}

/// RegisterChain governance VAAs targeted at Wormchain (the retiring cosmwasm accountant's
/// chain) are accepted during the wormchain -> Solana migration window; see
/// `ACCEPTED_REGISTER_CHAIN_TARGETS`.
#[test]
fn register_wormchain_target_accepted_during_migration_window() {
    let mollusk = mollusk();
    let emitter_a = [0x77u8; 32];

    let wormchain_target = governance_header(
        TOKEN_BRIDGE_GOVERNANCE_MODULE,
        REGISTER_CHAIN_ACTION,
        WORMCHAIN,
    );
    let body = register_chain_body(
        SOLANA_CHAIN_ID,
        GOVERNANCE_EMITTER,
        20,
        wormchain_target,
        ETHEREUM,
        emitter_a,
    );
    let registration = Registration::with_body(20, ETHEREUM, body);
    let result = registration.submit(
        &mollusk,
        registration.accounts(uninitialised_pda_account(), noreplay_bucket_unmarked()),
    );
    assert_success(&result, "wormchain-targeted registration");
    let account = find_account(&result.resulting_accounts, &registration.registration_pda);
    assert_eq!(
        registration_layout(account),
        ChainRegistrationLayout::new(ETHEREUM, emitter_a, 20)
    );
}
