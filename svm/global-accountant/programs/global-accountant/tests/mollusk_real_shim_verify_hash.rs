//! Real Verify VAA Shim `VerifyHash` CPI anchor: drives `close_digest` against
//! a Mollusk with the real `wormhole_verify_vaa_shim.so`. The sub-quorum case
//! is the anchor — it passes under mock-vaa (short-circuits to Ok) but must
//! fail under the real Shim.

#![allow(clippy::too_many_arguments)]

use {
    global_accountant_definitions::{
        DigestAccountLayout, Instruction as IxDiscriminator, DIGEST_SEED_PREFIX,
        VERIFY_VAA_SHIM_PROGRAM_ID,
    },
    mollusk_svm::{program::keyed_account_for_system_program, result::ProgramResult, Mollusk},
    solana_account::Account,
    solana_instruction::{AccountMeta, Instruction},
    solana_pubkey::Pubkey,
};

mod common;
use common::guardian_fixtures::{
    derive_guardian_set_pda, guardian_set_account, guardian_signatures_account, make_guardians,
    sign_digest, GUARDIAN_PUBKEY_LENGTH,
};
use common::mollusk_fixtures::{keyed_account_for_verify_vaa_shim_program, mollusk_with_fixtures};

const PROGRAM_NAME: &str = "global_accountant";
const CHAIN: u16 = 2;
const SEQUENCE: u64 = 99;
const GUARDIAN_SET_INDEX: u32 = 4;
const GUARDIAN_COUNT: usize = 19;
const QUORUM: u8 = 13;

fn program_id() -> Pubkey {
    Pubkey::new_from_array([7u8; 32])
}

fn system_program_id() -> Pubkey {
    keyed_account_for_system_program().0
}

fn core_bridge_program_id() -> Pubkey {
    Pubkey::new_from_array(global_accountant_definitions::CORE_BRIDGE_PROGRAM_ID)
}

fn shim_program_id() -> Pubkey {
    Pubkey::new_from_array(VERIFY_VAA_SHIM_PROGRAM_ID)
}

fn derive_digest_pda(chain: u16, emitter: &[u8; 32], sequence: u64) -> (Pubkey, u8) {
    let chain_be = chain.to_be_bytes();
    let sequence_be = sequence.to_be_bytes();
    Pubkey::find_program_address(
        &[DIGEST_SEED_PREFIX, &chain_be, emitter, &sequence_be],
        &program_id(),
    )
}

fn open_digest_ix_data(
    chain: u16,
    emitter: &[u8; 32],
    sequence: u64,
    digest: &[u8; 32],
    guardian_set_index: u32,
) -> Vec<u8> {
    // No bump on the wire; the program derives it on-chain.
    let mut data = Vec::with_capacity(1 + 78);
    data.push(IxDiscriminator::TestOnlyOpenDigest as u8);
    data.extend_from_slice(&chain.to_be_bytes());
    data.extend_from_slice(emitter);
    data.extend_from_slice(&sequence.to_be_bytes());
    data.extend_from_slice(digest);
    data.extend_from_slice(&guardian_set_index.to_le_bytes());
    data
}

fn close_digest_ix_data(digest: &[u8; 32], guardian_set_bump: u8) -> Vec<u8> {
    let mut data = Vec::with_capacity(1 + 32 + 1);
    data.push(IxDiscriminator::CloseDigest as u8);
    data.extend_from_slice(digest);
    data.push(guardian_set_bump);
    data
}

fn system_owned_account(lamports: u64) -> Account {
    Account {
        lamports,
        data: vec![],
        owner: system_program_id(),
        executable: false,
        rent_epoch: 0,
    }
}

fn payer_account(lamports: u64) -> Account {
    system_owned_account(lamports)
}

fn uninit_pda_account() -> Account {
    system_owned_account(0)
}

/// Post-open Mollusk + accounts shared by the happy-path and sub-quorum tests.
struct CloseFixture {
    mollusk: Mollusk,
    digest: [u8; 32],
    digest_pda: Pubkey,
    digest_pda_account: Account,
    payer: Pubkey,
    payer_account: Account,
    guardian_set_pubkey: Pubkey,
    guardian_set_bump: u8,
    guardians: Vec<common::guardian_fixtures::Guardian>,
}

impl CloseFixture {
    fn open() -> Self {
        let mollusk = mollusk_with_fixtures(&program_id(), PROGRAM_NAME);

        // Any 32 bytes work; the Shim signs whatever digest we hand it.
        let mut digest = [0u8; 32];
        for (i, b) in digest.iter_mut().enumerate() {
            *b = (i as u8) ^ 0x5A;
        }
        let mut emitter = [0u8; 32];
        emitter[31] = 0x77;
        let (digest_pda, _) = derive_digest_pda(CHAIN, &emitter, SEQUENCE);
        let payer = Pubkey::new_from_array([1u8; 32]);

        let open_ix = Instruction::new_with_bytes(
            program_id(),
            &open_digest_ix_data(CHAIN, &emitter, SEQUENCE, &digest, GUARDIAN_SET_INDEX),
            vec![
                AccountMeta::new(payer, true),
                AccountMeta::new(digest_pda, false),
                AccountMeta::new_readonly(system_program_id(), false),
            ],
        );
        let open_accounts = vec![
            (payer, payer_account(10_000_000_000)),
            (digest_pda, uninit_pda_account()),
            keyed_account_for_system_program(),
        ];
        let open_result = mollusk.process_instruction(&open_ix, &open_accounts);
        assert!(
            matches!(open_result.program_result, ProgramResult::Success),
            "open_digest failed: {:?}",
            open_result.program_result
        );

        let digest_pda_account = open_result
            .resulting_accounts
            .iter()
            .find(|(k, _)| *k == digest_pda)
            .expect("digest PDA in open result")
            .1
            .clone();
        let payer_account_after = open_result
            .resulting_accounts
            .iter()
            .find(|(k, _)| *k == payer)
            .expect("payer in open result")
            .1
            .clone();

        let stored: &DigestAccountLayout = bytemuck::from_bytes(&digest_pda_account.data);
        assert_eq!(stored.digest, digest, "stored digest matches input");

        let guardians = make_guardians(GUARDIAN_COUNT, 0x42);
        let (guardian_set_pubkey, guardian_set_bump) =
            derive_guardian_set_pda(GUARDIAN_SET_INDEX, &core_bridge_program_id());

        Self {
            mollusk,
            digest,
            digest_pda,
            digest_pda_account,
            payer,
            payer_account: payer_account_after,
            guardian_set_pubkey,
            guardian_set_bump,
            guardians,
        }
    }

    fn guardian_set_account(&self) -> Account {
        let keys: Vec<[u8; GUARDIAN_PUBKEY_LENGTH]> =
            self.guardians.iter().map(|g| g.eth_address).collect();
        // `expiration_time = 0` ⇒ never expires.
        guardian_set_account(GUARDIAN_SET_INDEX, &keys, 0, 0, &core_bridge_program_id())
    }

    /// GuardianSignatures fixture: `num_signatures` guardians signing the stored
    /// digest in strictly increasing index order (the Shim requires it).
    fn guardian_signatures_account(&self, num_signatures: u8) -> (Pubkey, Account) {
        let pubkey = Pubkey::new_from_array([0xE1u8; 32]);
        let sigs: Vec<(u8, [u8; 65])> = (0..num_signatures)
            .map(|i| (i, sign_digest(&self.guardians[i as usize], &self.digest)))
            .collect();
        let account =
            guardian_signatures_account(GUARDIAN_SET_INDEX, &self.payer, &sigs, &shim_program_id());
        (pubkey, account)
    }

    fn close_with(
        &self,
        sigs_pubkey: Pubkey,
        sigs_account: Account,
    ) -> mollusk_svm::result::InstructionResult {
        // Production close_digest meta order: closer, digest_pda, rent_recipient,
        // guardian_signatures, guardian_set, shim_program. close_digest::verify_vaa
        // reorders for the Shim (which reads guardian_set first) in the CPI list.
        let metas = vec![
            AccountMeta::new_readonly(self.payer, true),
            AccountMeta::new(self.digest_pda, false),
            AccountMeta::new(self.payer, false),
            AccountMeta::new_readonly(sigs_pubkey, false),
            AccountMeta::new_readonly(self.guardian_set_pubkey, false),
            AccountMeta::new_readonly(shim_program_id(), false),
        ];
        let close_ix = Instruction::new_with_bytes(
            program_id(),
            &close_digest_ix_data(&self.digest, self.guardian_set_bump),
            metas,
        );
        let close_accounts = vec![
            (self.payer, self.payer_account.clone()),
            (self.digest_pda, self.digest_pda_account.clone()),
            (sigs_pubkey, sigs_account),
            (self.guardian_set_pubkey, self.guardian_set_account()),
            keyed_account_for_verify_vaa_shim_program(),
        ];
        self.mollusk.process_instruction(&close_ix, &close_accounts)
    }
}

/// Happy path: close succeeds with a quorum of valid signatures. Pins the wire
/// shape; not the anchor (also passes under mock-vaa).
#[test]
fn close_digest_with_quorum_signatures_succeeds() {
    let fixture = CloseFixture::open();
    let (sigs_pubkey, sigs_account) = fixture.guardian_signatures_account(QUORUM);
    let result = fixture.close_with(sigs_pubkey, sigs_account);
    assert!(
        matches!(result.program_result, ProgramResult::Success),
        "close_digest with quorum sigs expected success, got {:?}",
        result.program_result
    );
}

/// Anchor: a sub-quorum GuardianSignatures account must make close fail via the
/// Shim's quorum check (mock-vaa would wrongly pass it).
#[test]
fn close_digest_with_sub_quorum_signatures_fails_via_shim() {
    let fixture = CloseFixture::open();
    let (sigs_pubkey, sigs_account) = fixture.guardian_signatures_account(QUORUM - 1);
    let result = fixture.close_with(sigs_pubkey, sigs_account);
    assert!(
        matches!(result.program_result, ProgramResult::Failure(_)),
        "close_digest with sub-quorum sigs must fail, got {:?}",
        result.program_result
    );
}
