use accountant_operational_core::accounts::chain_registration;
use accountant_operational_core::cpi::noreplay::derive_bucket_pda;
use accountant_operational_core::support::pda;
use global_accountant_definitions::{
    GlobalAccountantError, ManagerMode, TransceiverHubLayout, TransceiverKey,
};
use mollusk_svm::program::keyed_account_for_system_program;
use mollusk_svm::result::InstructionResult;
use mollusk_svm::Mollusk;
use solana_account::Account;
use solana_instruction::AccountMeta;
use solana_pubkey::Pubkey;

use crate::common::*;

struct Hub {
    vaa: SignedVaa,
    sequence: u64,
    relayer_registration_pda: Pubkey,
    hub_pda: Pubkey,
    noreplay_bucket: Pubkey,
}

impl Hub {
    /// Published directly by `SPOKE`.
    fn direct(sequence: u64, mode: ManagerMode) -> Self {
        Self::with_body(
            SPOKE,
            sequence,
            direct_body(ETHEREUM, SPOKE, sequence, &hub_payload(mode)),
        )
    }

    /// Published by `RELAYER`, wrapping `SPOKE`'s message.
    fn relayed(sequence: u64, mode: ManagerMode) -> Self {
        Self::with_body(
            RELAYER,
            sequence,
            relayed_body(ETHEREUM, RELAYER, sequence, SPOKE, &hub_payload(mode)),
        )
    }

    fn with_body(emitter: [u8; 32], sequence: u64, body: Vec<u8>) -> Self {
        Self {
            vaa: SignedVaa::new(body),
            sequence,
            relayer_registration_pda: chain_registration::derive_pda(&program_id(), ETHEREUM).0,
            hub_pda: pda::derive(&program_id(), &TransceiverKey::new(ETHEREUM, SPOKE)).0,
            noreplay_bucket: derive_bucket_pda(
                &noreplay_authority_pda(&program_id()),
                ETHEREUM,
                &emitter,
                sequence,
            )
            .0,
        }
    }

    fn submit(
        &self,
        mollusk: &Mollusk,
        relayer_registration: Account,
        hub: Account,
        bucket: Account,
    ) -> InstructionResult {
        self.vaa.submit(
            mollusk,
            program_id(),
            &register_hub_ix_data(self.vaa.guardian_set_bump, &self.vaa.body),
            vec![
                AccountMeta::new_readonly(self.relayer_registration_pda, false),
                AccountMeta::new(self.hub_pda, false),
                AccountMeta::new(self.noreplay_bucket, false),
                AccountMeta::new_readonly(noreplay_program_id(), false),
                AccountMeta::new_readonly(noreplay_authority_pda(&program_id()), false),
                AccountMeta::new_readonly(system_program_id(), false),
            ],
            vec![
                (self.relayer_registration_pda, relayer_registration),
                (self.hub_pda, hub),
                (self.noreplay_bucket, bucket),
                keyed_account_for_noreplay_program(),
                (
                    noreplay_authority_pda(&program_id()),
                    system_owned_account(0),
                ),
                keyed_account_for_system_program(),
            ],
        )
    }
}

/// Ethereum's relayer registration naming `RELAYER`.
fn relayer_registered() -> Account {
    chain_registration_account_for(&program_id(), ETHEREUM, RELAYER)
}

fn assert_self_hub_created(result: &InstructionResult, hub: &Hub) {
    let account = find_account(&result.resulting_accounts, &hub.hub_pda);
    assert_eq!(account.owner, program_id());
    assert_eq!(
        *bytemuck::from_bytes::<TransceiverHubLayout>(&account.data),
        TransceiverHubLayout::new(
            TransceiverKey::new(ETHEREUM, SPOKE),
            TransceiverKey::new(ETHEREUM, SPOKE),
        ),
        "hub points at itself"
    );
    assert_bucket_marked(
        find_account(&result.resulting_accounts, &hub.noreplay_bucket),
        hub.sequence,
    );
}

#[test]
fn locking_info_registers_a_self_hub() {
    let mollusk = mollusk();
    let hub = Hub::direct(6, ManagerMode::Locking);
    let result = hub.submit(
        &mollusk,
        uninitialised_pda_account(),
        uninitialised_pda_account(),
        noreplay_bucket_unmarked(),
    );
    assert_success(&result, "register hub");
    assert_self_hub_created(&result, &hub);
}

#[test]
fn relayed_locking_info_registers_the_inner_sender_as_hub() {
    let mollusk = mollusk();
    let hub = Hub::relayed(6, ManagerMode::Locking);
    let result = hub.submit(
        &mollusk,
        relayer_registered(),
        uninitialised_pda_account(),
        noreplay_bucket_unmarked(),
    );
    assert_success(&result, "register hub via relayer");
    assert_self_hub_created(&result, &hub);
}

#[test]
fn rejects() {
    let mollusk = mollusk();
    let existing = hub_account(&TransceiverHubLayout::new(
        TransceiverKey::new(ETHEREUM, SPOKE),
        TransceiverKey::new(ETHEREUM, SPOKE),
    ));
    let mut short = direct_body(ETHEREUM, SPOKE, 10, &hub_payload(ManagerMode::Locking));
    short.pop();
    let mut long = direct_body(ETHEREUM, SPOKE, 11, &hub_payload(ManagerMode::Locking));
    long.push(0);
    let mut spoofed_pda = Hub::direct(12, ManagerMode::Locking);
    spoofed_pda.hub_pda = Pubkey::new_unique();
    let mut spoofed_registration = Hub::direct(13, ManagerMode::Locking);
    spoofed_registration.relayer_registration_pda = Pubkey::new_unique();
    let garbage_envelope = Hub::with_body(
        RELAYER,
        15,
        direct_body(ETHEREUM, RELAYER, 15, &hub_payload(ManagerMode::Locking)),
    );

    let cases: [(&str, Hub, Account, Account, Account, GlobalAccountantError); 10] = [
        (
            "burning mode",
            Hub::direct(7, ManagerMode::Burning),
            uninitialised_pda_account(),
            uninitialised_pda_account(),
            noreplay_bucket_unmarked(),
            GlobalAccountantError::NotLockingHub,
        ),
        (
            "duplicate hub",
            Hub::direct(8, ManagerMode::Locking),
            uninitialised_pda_account(),
            existing,
            noreplay_bucket_unmarked(),
            GlobalAccountantError::DuplicateTransceiverHub,
        ),
        (
            "replayed sequence",
            Hub::direct(9, ManagerMode::Locking),
            uninitialised_pda_account(),
            uninitialised_pda_account(),
            noreplay_bucket_marked(9),
            GlobalAccountantError::AlreadyAccounted,
        ),
        (
            "body one byte short",
            Hub::with_body(SPOKE, 10, short),
            uninitialised_pda_account(),
            uninitialised_pda_account(),
            noreplay_bucket_unmarked(),
            GlobalAccountantError::MalformedNttMessage,
        ),
        (
            "body one byte long",
            Hub::with_body(SPOKE, 11, long),
            uninitialised_pda_account(),
            uninitialised_pda_account(),
            noreplay_bucket_unmarked(),
            GlobalAccountantError::MalformedNttMessage,
        ),
        (
            "non-canonical hub pda",
            spoofed_pda,
            uninitialised_pda_account(),
            uninitialised_pda_account(),
            noreplay_bucket_unmarked(),
            GlobalAccountantError::InvalidPda,
        ),
        (
            "non-canonical relayer registration pda",
            spoofed_registration,
            uninitialised_pda_account(),
            uninitialised_pda_account(),
            noreplay_bucket_unmarked(),
            GlobalAccountantError::InvalidPda,
        ),
        (
            "relayed envelope from an unregistered relayer",
            Hub::relayed(14, ManagerMode::Locking),
            uninitialised_pda_account(),
            uninitialised_pda_account(),
            noreplay_bucket_unmarked(),
            GlobalAccountantError::MalformedNttMessage,
        ),
        (
            "registered relayer without an envelope",
            garbage_envelope,
            relayer_registered(),
            uninitialised_pda_account(),
            noreplay_bucket_unmarked(),
            GlobalAccountantError::MalformedDeliveryInstruction,
        ),
        (
            "relayed burning mode",
            Hub::relayed(16, ManagerMode::Burning),
            relayer_registered(),
            uninitialised_pda_account(),
            noreplay_bucket_unmarked(),
            GlobalAccountantError::NotLockingHub,
        ),
    ];
    for (label, hub, relayer_registration, hub_account, bucket, expected) in cases {
        let result = hub.submit(&mollusk, relayer_registration, hub_account, bucket);
        assert_error(&result, expected as u64, label);
        let untouched = find_account(&result.resulting_accounts, &hub.hub_pda);
        assert_eq!(
            untouched.owner == program_id(),
            label == "duplicate hub",
            "{label}: hub pda unchanged"
        );
    }
}
