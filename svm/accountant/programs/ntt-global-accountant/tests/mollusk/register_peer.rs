//! `register_peer` follows the CosmWasm setup order: a hub pre-registers a spoke (step 5),
//! the spoke adopts the hub that registered it (step 6), spokes under one hub cross-register
//! (step 7). Every other `(sender_hub, peer_hub)` combination is rejected.

use accountant_operational_core::accounts::chain_registration;
use accountant_operational_core::cpi::noreplay::derive_bucket_pda;
use accountant_operational_core::support::pda;
use global_accountant_definitions::{GlobalAccountantError, TransceiverHubKey, TransceiverPeerKey};
use mollusk_svm::program::keyed_account_for_system_program;
use mollusk_svm::result::InstructionResult;
use mollusk_svm::Mollusk;
use solana_account::Account;
use solana_instruction::AccountMeta;
use solana_pubkey::Pubkey;

use crate::common::*;

/// One registration VAA and the PDAs its handler expects.
struct Registration {
    vaa: SignedVaa,
    sequence: u64,
    emitter: [u8; 32],
    relayer_registration_pda: Pubkey,
    own_hub_pda: Pubkey,
    peer_hub_pda: Pubkey,
    hub_peer_pda: Pubkey,
    peer_pda: Pubkey,
    noreplay_bucket: Pubkey,
}

impl Registration {
    /// `sender` on `chain` publishes directly: it registers `peer` on `dest_chain`.
    fn direct(
        sequence: u64,
        chain: u16,
        sender: [u8; 32],
        dest_chain: u16,
        peer: [u8; 32],
    ) -> Self {
        let body = direct_body(chain, sender, sequence, &peer_payload(dest_chain, peer));
        Self::build(sequence, chain, sender, sender, dest_chain, peer, body)
    }

    /// `relayer` on `chain` publishes a `DeliveryInstruction` from `sender`.
    fn relayed(
        sequence: u64,
        chain: u16,
        relayer: [u8; 32],
        sender: [u8; 32],
        dest_chain: u16,
        peer: [u8; 32],
    ) -> Self {
        let body = relayed_body(
            chain,
            relayer,
            sequence,
            sender,
            &peer_payload(dest_chain, peer),
        );
        Self::build(sequence, chain, relayer, sender, dest_chain, peer, body)
    }

    fn build(
        sequence: u64,
        chain: u16,
        emitter: [u8; 32],
        sender: [u8; 32],
        dest_chain: u16,
        peer: [u8; 32],
        body: Vec<u8>,
    ) -> Self {
        let id = program_id();
        Self {
            vaa: SignedVaa::new(body),
            sequence,
            emitter,
            relayer_registration_pda: chain_registration::derive_pda(&id, chain).0,
            own_hub_pda: pda::derive(&id, &TransceiverHubKey::new(chain, sender)).0,
            peer_hub_pda: pda::derive(&id, &TransceiverHubKey::new(dest_chain, peer)).0,
            hub_peer_pda: pda::derive(&id, &TransceiverPeerKey::new(dest_chain, peer, chain)).0,
            peer_pda: pda::derive(&id, &TransceiverPeerKey::new(chain, sender, dest_chain)).0,
            noreplay_bucket: derive_bucket_pda(
                &noreplay_authority_pda(&id),
                chain,
                &emitter,
                sequence,
            )
            .0,
        }
    }

    fn submit(&self, mollusk: &Mollusk, accounts: Accounts) -> InstructionResult {
        self.vaa.submit(
            mollusk,
            program_id(),
            &register_peer_ix_data(self.vaa.guardian_set_bump, &self.vaa.body),
            vec![
                AccountMeta::new_readonly(self.relayer_registration_pda, false),
                AccountMeta::new(self.own_hub_pda, false),
                AccountMeta::new_readonly(self.peer_hub_pda, false),
                AccountMeta::new_readonly(self.hub_peer_pda, false),
                AccountMeta::new(self.peer_pda, false),
                AccountMeta::new(self.noreplay_bucket, false),
                AccountMeta::new_readonly(noreplay_program_id(), false),
                AccountMeta::new_readonly(noreplay_authority_pda(&program_id()), false),
                AccountMeta::new_readonly(system_program_id(), false),
            ],
            vec![
                (self.relayer_registration_pda, accounts.relayer_registration),
                (self.own_hub_pda, accounts.own_hub),
                (self.peer_hub_pda, accounts.peer_hub),
                (self.hub_peer_pda, accounts.hub_peer),
                (self.peer_pda, accounts.peer),
                (self.noreplay_bucket, accounts.bucket),
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

/// Input state for the six handler-owned slots; everything uninitialised by default.
#[derive(Clone)]
struct Accounts {
    relayer_registration: Account,
    own_hub: Account,
    peer_hub: Account,
    hub_peer: Account,
    peer: Account,
    bucket: Account,
}

impl Default for Accounts {
    fn default() -> Self {
        Self {
            relayer_registration: uninitialised_pda_account(),
            own_hub: uninitialised_pda_account(),
            peer_hub: uninitialised_pda_account(),
            hub_peer: uninitialised_pda_account(),
            peer: uninitialised_pda_account(),
            bucket: noreplay_bucket_unmarked(),
        }
    }
}

fn self_hub(chain: u16, address: [u8; 32]) -> Account {
    hub_account(&hub_layout(chain, address, chain, address))
}

fn hub_under(chain: u16, address: [u8; 32], hub_chain: u16, hub: [u8; 32]) -> Account {
    hub_account(&hub_layout(chain, address, hub_chain, hub))
}

fn peer_entry(chain: u16, address: [u8; 32], dest_chain: u16, peer: [u8; 32]) -> Account {
    peer_account(&peer_layout(chain, address, dest_chain, peer))
}

/// Step 6 inputs: the hub exists and has registered `SPOKE` as its Ethereum peer.
fn hub_acknowledged_spoke() -> Accounts {
    Accounts {
        peer_hub: self_hub(SOLANA, HUB),
        hub_peer: peer_entry(SOLANA, HUB, ETHEREUM, SPOKE),
        ..Accounts::default()
    }
}

fn assert_layout<T: bytemuck::Pod + PartialEq + std::fmt::Debug>(
    result: &InstructionResult,
    pda: &Pubkey,
    expected: T,
    label: &str,
) {
    let account = find_account(&result.resulting_accounts, pda);
    assert_eq!(account.owner, program_id(), "{label}: owner");
    assert_eq!(
        *bytemuck::from_bytes::<T>(&account.data),
        expected,
        "{label}"
    );
}

#[test]
fn hub_pre_registers_a_hubless_spoke() {
    let mollusk = mollusk();
    let registration = Registration::direct(6, SOLANA, HUB, ETHEREUM, SPOKE);
    let own_hub = self_hub(SOLANA, HUB);
    let result = registration.submit(
        &mollusk,
        Accounts {
            own_hub: own_hub.clone(),
            ..Accounts::default()
        },
    );
    assert_success(&result, "hub registers spoke");
    assert_layout(
        &result,
        &registration.peer_pda,
        peer_layout(SOLANA, HUB, ETHEREUM, SPOKE),
        "peer entry",
    );
    assert_eq!(
        find_account(&result.resulting_accounts, &registration.own_hub_pda),
        &own_hub,
        "hub entry untouched"
    );
    assert_bucket_marked(
        find_account(&result.resulting_accounts, &registration.noreplay_bucket),
        registration.sequence,
    );
}

#[test]
fn spoke_adopts_the_hub_that_registered_it() {
    let mollusk = mollusk();
    let registration = Registration::direct(7, ETHEREUM, SPOKE, SOLANA, HUB);
    let result = registration.submit(&mollusk, hub_acknowledged_spoke());
    assert_success(&result, "spoke adopts hub");
    assert_layout(
        &result,
        &registration.own_hub_pda,
        hub_layout(ETHEREUM, SPOKE, SOLANA, HUB),
        "adopted hub",
    );
    assert_layout(
        &result,
        &registration.peer_pda,
        peer_layout(ETHEREUM, SPOKE, SOLANA, HUB),
        "peer entry",
    );
}

#[test]
fn relayed_registration_keys_on_the_inner_sender() {
    let mollusk = mollusk();
    let registration = Registration::relayed(8, ETHEREUM, RELAYER, SPOKE, SOLANA, HUB);
    let result = registration.submit(
        &mollusk,
        Accounts {
            relayer_registration: chain_registration_account_for(&program_id(), ETHEREUM, RELAYER),
            ..hub_acknowledged_spoke()
        },
    );
    assert_success(&result, "relayed adoption");
    assert_layout(
        &result,
        &registration.own_hub_pda,
        hub_layout(ETHEREUM, SPOKE, SOLANA, HUB),
        "adopted hub keyed on the sender",
    );
    assert_eq!(registration.emitter, RELAYER);
    assert_bucket_marked(
        find_account(&result.resulting_accounts, &registration.noreplay_bucket),
        registration.sequence,
    );
}

#[test]
fn spokes_under_one_hub_cross_register() {
    let mollusk = mollusk();
    let registration = Registration::direct(9, ETHEREUM, SPOKE, POLYGON, OTHER);
    let result = registration.submit(
        &mollusk,
        Accounts {
            own_hub: hub_under(ETHEREUM, SPOKE, SOLANA, HUB),
            peer_hub: hub_under(POLYGON, OTHER, SOLANA, HUB),
            ..Accounts::default()
        },
    );
    assert_success(&result, "cross-register");
    assert_layout(
        &result,
        &registration.peer_pda,
        peer_layout(ETHEREUM, SPOKE, POLYGON, OTHER),
        "peer entry",
    );
}

#[test]
fn rejects() {
    let mollusk = mollusk();
    let spoke_under_hub = hub_under(ETHEREUM, SPOKE, SOLANA, HUB);

    let mut spoofed_own_hub = Registration::direct(29, ETHEREUM, SPOKE, SOLANA, HUB);
    spoofed_own_hub.own_hub_pda = Pubkey::new_unique();
    let mut spoofed_peer = Registration::direct(30, ETHEREUM, SPOKE, SOLANA, HUB);
    spoofed_peer.peer_pda = Pubkey::new_unique();
    let mut spoofed_hub_peer = Registration::direct(31, ETHEREUM, SPOKE, SOLANA, HUB);
    spoofed_hub_peer.hub_peer_pda = Pubkey::new_unique();
    let mut short = direct_body(ETHEREUM, SPOKE, 33, &peer_payload(SOLANA, HUB));
    short.pop();

    type Row = (&'static str, Registration, Accounts, GlobalAccountantError);
    let cases: [Row; 15] = [
        (
            "neither side has a hub",
            Registration::direct(20, ETHEREUM, SPOKE, SOLANA, HUB),
            Accounts::default(),
            GlobalAccountantError::MissingTransceiverHub,
        ),
        (
            "spoke pre-registers a hubless peer",
            Registration::direct(21, ETHEREUM, SPOKE, POLYGON, OTHER),
            Accounts {
                own_hub: spoke_under_hub.clone(),
                ..Accounts::default()
            },
            GlobalAccountantError::HublessPeerRequiresHub,
        ),
        (
            "peer is a spoke and sender has no hub",
            Registration::direct(22, ETHEREUM, SPOKE, POLYGON, OTHER),
            Accounts {
                peer_hub: hub_under(POLYGON, OTHER, SOLANA, HUB),
                ..Accounts::default()
            },
            GlobalAccountantError::PeerBeforeHub,
        ),
        (
            "hub has not registered the sender",
            Registration::direct(23, ETHEREUM, SPOKE, SOLANA, HUB),
            Accounts {
                peer_hub: self_hub(SOLANA, HUB),
                ..Accounts::default()
            },
            GlobalAccountantError::HubHasNotRegisteredPeer,
        ),
        (
            "hub registered a different transceiver on the sender chain",
            Registration::direct(24, ETHEREUM, SPOKE, SOLANA, HUB),
            Accounts {
                hub_peer: peer_entry(SOLANA, HUB, ETHEREUM, OTHER),
                ..hub_acknowledged_spoke()
            },
            GlobalAccountantError::HubHasNotRegisteredPeer,
        ),
        (
            "hubs differ",
            Registration::direct(25, ETHEREUM, SPOKE, POLYGON, OTHER),
            Accounts {
                own_hub: spoke_under_hub.clone(),
                peer_hub: self_hub(POLYGON, OTHER),
                ..Accounts::default()
            },
            GlobalAccountantError::PeerRegistrationMismatch,
        ),
        (
            "peer on the sender chain",
            Registration::direct(26, ETHEREUM, SPOKE, ETHEREUM, OTHER),
            Accounts {
                own_hub: spoke_under_hub.clone(),
                ..Accounts::default()
            },
            GlobalAccountantError::SameChainPeer,
        ),
        (
            "duplicate peer",
            Registration::direct(27, ETHEREUM, SPOKE, SOLANA, HUB),
            Accounts {
                peer: peer_entry(ETHEREUM, SPOKE, SOLANA, HUB),
                ..hub_acknowledged_spoke()
            },
            GlobalAccountantError::DuplicateTransceiverPeer,
        ),
        (
            "replayed sequence",
            Registration::direct(28, ETHEREUM, SPOKE, SOLANA, HUB),
            Accounts {
                bucket: noreplay_bucket_marked(28),
                ..hub_acknowledged_spoke()
            },
            GlobalAccountantError::AlreadyAccounted,
        ),
        (
            "non-canonical own hub pda",
            spoofed_own_hub,
            hub_acknowledged_spoke(),
            GlobalAccountantError::InvalidPda,
        ),
        (
            "non-canonical peer pda",
            spoofed_peer,
            hub_acknowledged_spoke(),
            GlobalAccountantError::InvalidPda,
        ),
        (
            "non-canonical hub peer pda",
            spoofed_hub_peer,
            hub_acknowledged_spoke(),
            GlobalAccountantError::InvalidPda,
        ),
        (
            "peer hub slot holds a peer layout",
            Registration::direct(32, ETHEREUM, SPOKE, SOLANA, HUB),
            Accounts {
                peer_hub: peer_entry(SOLANA, HUB, ETHEREUM, SPOKE),
                ..hub_acknowledged_spoke()
            },
            GlobalAccountantError::InvalidPda,
        ),
        (
            "body one byte short",
            Registration::build(33, ETHEREUM, SPOKE, SPOKE, SOLANA, HUB, short),
            hub_acknowledged_spoke(),
            GlobalAccountantError::MalformedNttMessage,
        ),
        (
            "relayed envelope from an unregistered relayer",
            Registration::relayed(34, ETHEREUM, RELAYER, SPOKE, SOLANA, HUB),
            hub_acknowledged_spoke(),
            GlobalAccountantError::MalformedNttMessage,
        ),
    ];
    for (label, registration, accounts, expected) in cases {
        let result = registration.submit(&mollusk, accounts.clone());
        assert_error(&result, expected as u64, label);
        assert_eq!(
            find_account(&result.resulting_accounts, &registration.own_hub_pda),
            &accounts.own_hub,
            "{label}: own hub unchanged"
        );
        assert_eq!(
            find_account(&result.resulting_accounts, &registration.peer_pda),
            &accounts.peer,
            "{label}: peer unchanged"
        );
    }
}
