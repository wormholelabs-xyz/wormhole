use accountant_operational_core::accounts::chain_registration;
use accountant_operational_core::cpi::noreplay::derive_bucket_pda;
use accountant_operational_core::support::quorum::derive_pending_pda;
use global_accountant::instructions::transfer::derive_balance_account_pda;
use global_accountant_definitions::Uint256;
use mollusk_svm::program::keyed_account_for_system_program;
use mollusk_svm::result::{InstructionResult, ProgramResult};
use mollusk_svm::Mollusk;
use solana_account::Account;
use solana_instruction::{AccountMeta, Instruction};
use solana_pubkey::Pubkey;

use super::accounts::*;
use super::guardians::*;
use super::ix::*;
use super::mollusk::*;

pub const GUARDIAN_COUNT: usize = 19;
pub const QUORUM: u8 = 13;
pub const GUARDIAN_SET_INDEX: u32 = 4;
pub const SOLANA: u16 = 1;
pub const ETHEREUM: u16 = 2;
pub const TOKEN_ADDRESS: [u8; 32] = [0x77u8; 32];
pub const SUBMITTER: Pubkey = Pubkey::new_from_array([0x11u8; 32]);
pub const GUARDIAN_SIGNATURES: Pubkey = Pubkey::new_from_array([0xC5u8; 32]);

pub fn emitter(seed: u8) -> [u8; 32] {
    let mut emitter = [0u8; 32];
    emitter[0] = seed;
    emitter[31] = 0x77;
    emitter
}

pub fn noreplay_authority() -> Pubkey {
    noreplay_authority_pda(&program_id())
}

pub fn error_code(result: &ProgramResult) -> Option<u64> {
    match result {
        ProgramResult::Failure(err) => Some(u64::from(err.clone())),
        _ => None,
    }
}

pub fn assert_success(result: &InstructionResult, label: &str) {
    assert!(
        matches!(result.program_result, ProgramResult::Success),
        "{label}: {:?}",
        result.program_result
    );
}

pub fn assert_error(result: &InstructionResult, expected: u64, label: &str) {
    assert_eq!(
        error_code(&result.program_result),
        Some(expected),
        "{label}: {:?}",
        result.program_result
    );
}

#[derive(Clone, Copy)]
pub struct Transfer {
    pub chain: u16,
    pub emitter: [u8; 32],
    pub sequence: u64,
    pub amount: u128,
    pub token_chain: u16,
    pub token_address: [u8; 32],
    pub recipient_chain: u16,
}

impl Transfer {
    pub fn new(seed: u8, from_chain: u16, to_chain: u16, amount: u128) -> Self {
        Self {
            chain: from_chain,
            emitter: emitter(seed),
            sequence: 0x42,
            amount,
            token_chain: ETHEREUM,
            token_address: TOKEN_ADDRESS,
            recipient_chain: to_chain,
        }
    }

    pub fn body(&self) -> Vec<u8> {
        transfer_body(
            self.chain,
            self.emitter,
            self.sequence,
            Uint256::from_u128(self.amount),
            self.token_chain,
            self.token_address,
            self.recipient_chain,
        )
    }

    pub fn source(&self) -> Pubkey {
        derive_balance_account_pda(
            &program_id(),
            self.chain,
            self.token_chain,
            &self.token_address,
        )
        .0
    }

    pub fn dest(&self) -> Pubkey {
        derive_balance_account_pda(
            &program_id(),
            self.recipient_chain,
            self.token_chain,
            &self.token_address,
        )
        .0
    }
}

pub fn signatures_for(guardians: &[Guardian], digest: &[u8; 32], count: u8) -> Vec<(u8, [u8; 65])> {
    (0..count)
        .map(|i| (i, sign_digest(&guardians[i as usize], digest)))
        .collect()
}

pub fn guardian_keys(guardians: &[Guardian]) -> Vec<[u8; GUARDIAN_PUBKEY_LENGTH]> {
    guardians.iter().map(|g| g.eth_address).collect()
}

#[derive(Clone)]
pub struct VaaScenario {
    pub chain: u16,
    pub emitter: [u8; 32],
    pub sequence: u64,
    pub body: Vec<u8>,
    pub guardian_set_bump: u8,
    pub guardian_set: Pubkey,
    pub noreplay_bucket: Pubkey,
    pub source_account: Pubkey,
    pub dest_account: Pubkey,
    pub chain_registration: Pubkey,
    pub guardians: Vec<Guardian>,
}

impl VaaScenario {
    pub fn transfer(transfer: Transfer) -> Self {
        let (guardian_set, guardian_set_bump) =
            derive_guardian_set_pda(GUARDIAN_SET_INDEX, &core_bridge_program_id());
        Self {
            chain: transfer.chain,
            emitter: transfer.emitter,
            sequence: transfer.sequence,
            body: transfer.body(),
            guardian_set_bump,
            guardian_set,
            noreplay_bucket: derive_bucket_pda(
                &noreplay_authority(),
                transfer.chain,
                &transfer.emitter,
                transfer.sequence,
            )
            .0,
            source_account: transfer.source(),
            dest_account: transfer.dest(),
            chain_registration: chain_registration::derive_pda(&program_id(), transfer.chain).0,
            guardians: make_guardians(GUARDIAN_COUNT, 0x42),
        }
    }

    pub fn account_metas(&self) -> Vec<AccountMeta> {
        vec![
            AccountMeta::new(SUBMITTER, true),
            AccountMeta::new_readonly(shim_program_id(), false),
            AccountMeta::new_readonly(self.guardian_set, false),
            AccountMeta::new_readonly(GUARDIAN_SIGNATURES, false),
            AccountMeta::new(self.noreplay_bucket, false),
            AccountMeta::new_readonly(noreplay_program_id(), false),
            AccountMeta::new_readonly(noreplay_authority(), false),
            AccountMeta::new(self.source_account, false),
            AccountMeta::new(self.dest_account, false),
            AccountMeta::new_readonly(system_program_id(), false),
            AccountMeta::new_readonly(self.chain_registration, false),
        ]
    }

    pub fn accounts(&self) -> Vec<(Pubkey, Account)> {
        let digest = double_keccak256(&self.body);
        vec![
            (SUBMITTER, system_owned_account(50_000_000_000)),
            keyed_account_for_verify_vaa_shim_program(),
            (
                self.guardian_set,
                guardian_set_account(
                    GUARDIAN_SET_INDEX,
                    &guardian_keys(&self.guardians),
                    0,
                    0,
                    &core_bridge_program_id(),
                ),
            ),
            (
                GUARDIAN_SIGNATURES,
                guardian_signatures_account(
                    GUARDIAN_SET_INDEX,
                    &SUBMITTER,
                    &signatures_for(&self.guardians, &digest, QUORUM),
                    &shim_program_id(),
                ),
            ),
            (self.noreplay_bucket, noreplay_bucket_unmarked()),
            keyed_account_for_noreplay_program(),
            (noreplay_authority(), system_owned_account(0)),
            (self.source_account, uninitialised_pda_account()),
            (self.dest_account, uninitialised_pda_account()),
            keyed_account_for_system_program(),
            (
                self.chain_registration,
                chain_registration_account(self.chain, self.emitter),
            ),
        ]
    }

    pub fn submit(&self, mollusk: &Mollusk, accounts: Vec<(Pubkey, Account)>) -> InstructionResult {
        self.submit_with(
            mollusk,
            accounts,
            submit_vaas_ix_data(self.guardian_set_bump, &self.body),
        )
    }

    pub fn submit_with(
        &self,
        mollusk: &Mollusk,
        accounts: Vec<(Pubkey, Account)>,
        ix_data: Vec<u8>,
    ) -> InstructionResult {
        let ix = Instruction::new_with_bytes(program_id(), &ix_data, self.account_metas());
        mollusk.process_instruction(&ix, &accounts)
    }
}

#[derive(Clone)]
pub struct ObsScenario {
    pub chain: u16,
    pub emitter: [u8; 32],
    pub sequence: u64,
    pub body: Vec<u8>,
    pub digest: [u8; 32],
    pub signing_digest: [u8; 32],
    pub guardian_set_index: u32,
    pub guardians: Vec<Guardian>,
    pub pending_pda: Pubkey,
    pub guardian_set: Pubkey,
    pub noreplay_bucket: Pubkey,
    pub source_account: Pubkey,
    pub dest_account: Pubkey,
    pub chain_registration: Pubkey,
}

impl ObsScenario {
    pub fn attest(guardian_count: usize, guardian_set_index: u32, seed: u8) -> Self {
        let chain = ETHEREUM;
        let emitter = emitter(0);
        let sequence = 0x42;
        let mut scenario = Self::empty(
            guardian_count,
            guardian_set_index,
            seed,
            chain,
            emitter,
            sequence,
        );
        scenario.set_body(attest_body(chain, emitter, sequence));
        scenario.source_account = noreplay_authority();
        scenario.dest_account = noreplay_authority();
        scenario
    }

    pub fn transfer(guardian_set_index: u32, seed: u8, transfer: Transfer) -> Self {
        let mut scenario = Self::empty(
            GUARDIAN_COUNT,
            guardian_set_index,
            seed,
            transfer.chain,
            transfer.emitter,
            transfer.sequence,
        );
        scenario.set_body(transfer.body());
        scenario.source_account = transfer.source();
        scenario.dest_account = transfer.dest();
        scenario
    }

    fn empty(
        guardian_count: usize,
        guardian_set_index: u32,
        seed: u8,
        chain: u16,
        emitter: [u8; 32],
        sequence: u64,
    ) -> Self {
        Self {
            chain,
            emitter,
            sequence,
            body: Vec::new(),
            digest: [0; 32],
            signing_digest: [0; 32],
            guardian_set_index,
            guardians: make_guardians(guardian_count, seed),
            pending_pda: Pubkey::default(),
            guardian_set: derive_guardian_set_pda(guardian_set_index, &core_bridge_program_id()).0,
            noreplay_bucket: derive_bucket_pda(&noreplay_authority(), chain, &emitter, sequence).0,
            source_account: Pubkey::default(),
            dest_account: Pubkey::default(),
            chain_registration: chain_registration::derive_pda(&program_id(), chain).0,
        }
    }

    pub fn set_body(&mut self, body: Vec<u8>) {
        self.digest = double_keccak256(&body);
        self.signing_digest = signing_digest(&body);
        self.pending_pda = derive_pending_pda(
            &program_id(),
            self.chain,
            &self.emitter,
            self.sequence,
            self.guardian_set_index,
            &self.digest,
        )
        .0;
        self.body = body;
    }

    pub fn account_metas(&self) -> Vec<AccountMeta> {
        vec![
            AccountMeta::new(SUBMITTER, true),
            AccountMeta::new(self.pending_pda, false),
            AccountMeta::new_readonly(self.guardian_set, false),
            AccountMeta::new(self.noreplay_bucket, false),
            AccountMeta::new_readonly(system_program_id(), false),
            AccountMeta::new_readonly(noreplay_program_id(), false),
            AccountMeta::new_readonly(noreplay_authority(), false),
            AccountMeta::new(self.source_account, false),
            AccountMeta::new(self.dest_account, false),
            AccountMeta::new(SUBMITTER, false),
            AccountMeta::new_readonly(self.chain_registration, false),
        ]
    }

    pub fn initial_accounts(&self) -> Vec<(Pubkey, Account)> {
        let mut accounts = vec![
            (SUBMITTER, system_owned_account(50_000_000_000)),
            (self.pending_pda, uninitialised_pda_account()),
            (
                self.guardian_set,
                guardian_set_account(
                    self.guardian_set_index,
                    &guardian_keys(&self.guardians),
                    0,
                    0,
                    &core_bridge_program_id(),
                ),
            ),
            (self.noreplay_bucket, noreplay_bucket_unmarked()),
            keyed_account_for_system_program(),
            keyed_account_for_noreplay_program(),
            (noreplay_authority(), system_owned_account(0)),
        ];
        if self.source_account != noreplay_authority() {
            accounts.push((self.source_account, uninitialised_pda_account()));
        }
        if self.dest_account != noreplay_authority() && self.dest_account != self.source_account {
            accounts.push((self.dest_account, uninitialised_pda_account()));
        }
        accounts.push((
            self.chain_registration,
            chain_registration_account(self.chain, self.emitter),
        ));
        accounts
    }

    pub fn ix_data(&self, guardian_index: u8) -> Vec<u8> {
        let signature = sign_digest(
            &self.guardians[guardian_index as usize],
            &self.signing_digest,
        );
        submit_observations_ix_data(
            self.guardian_set_index,
            guardian_index,
            signature,
            &self.body,
        )
    }

    pub fn submit_once(
        &self,
        mollusk: &Mollusk,
        accounts: Vec<(Pubkey, Account)>,
        guardian_index: u8,
    ) -> InstructionResult {
        let ix = Instruction::new_with_bytes(
            program_id(),
            &self.ix_data(guardian_index),
            self.account_metas(),
        );
        mollusk.process_instruction(&ix, &accounts)
    }

    pub fn submit_n(&self, mollusk: &Mollusk, n: u8) -> Vec<(Pubkey, Account)> {
        self.submit_range(mollusk, self.initial_accounts(), 0..n)
    }

    pub fn submit_range(
        &self,
        mollusk: &Mollusk,
        mut accounts: Vec<(Pubkey, Account)>,
        range: std::ops::Range<u8>,
    ) -> Vec<(Pubkey, Account)> {
        for i in range {
            let result = self.submit_once(mollusk, accounts, i);
            assert_success(&result, &format!("observation {i}"));
            accounts = result.resulting_accounts;
        }
        accounts
    }
}
