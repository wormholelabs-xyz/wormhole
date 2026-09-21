use accountant_operational_core::accounts::{balance, chain_registration};
use accountant_operational_core::cpi::noreplay::derive_bucket_pda;
use accountant_operational_core::support::pda;
use accountant_operational_core::support::quorum::{
    derive_pending_pda, observation_digests, ObservationDigests,
};
use accountant_test_harness::wire;
use global_accountant_definitions::{
    NttSubmitObservationsIxData, TransceiverHubKey, TransceiverPeerKey,
    NTT_SUBMIT_OBSERVATION_PREFIX,
};
use mollusk_svm::program::keyed_account_for_system_program;
use mollusk_svm::result::InstructionResult;
use mollusk_svm::Mollusk;
use solana_account::Account;
use solana_instruction::{AccountMeta, Instruction};
use solana_pubkey::Pubkey;

use super::*;

/// PDAs both transfer handlers take for `sender` on `chain` under `hub`, cross-registered
/// with `peer` on `recipient_chain`, published under `(emitter, sequence)`.
struct RoutePdas {
    noreplay_bucket: Pubkey,
    source_balance: Pubkey,
    dest_balance: Pubkey,
    relayer_registration_pda: Pubkey,
    hub_pda: Pubkey,
    peer_src_pda: Pubkey,
    peer_dst_pda: Pubkey,
}

#[allow(clippy::too_many_arguments)]
fn route_pdas(
    chain: u16,
    emitter: [u8; 32],
    sequence: u64,
    sender: [u8; 32],
    recipient_chain: u16,
    peer: [u8; 32],
    (hub_chain, hub): (u16, [u8; 32]),
) -> RoutePdas {
    let id = program_id();
    RoutePdas {
        noreplay_bucket: derive_bucket_pda(&noreplay_authority_pda(&id), chain, &emitter, sequence)
            .0,
        source_balance: balance::derive_pda(&id, chain, hub_chain, &hub).0,
        dest_balance: balance::derive_pda(&id, recipient_chain, hub_chain, &hub).0,
        relayer_registration_pda: chain_registration::derive_pda(&id, chain).0,
        hub_pda: pda::derive(&id, &TransceiverHubKey::new(chain, sender)).0,
        peer_src_pda: pda::derive(
            &id,
            &TransceiverPeerKey::new(chain, sender, recipient_chain),
        )
        .0,
        peer_dst_pda: pda::derive(&id, &TransceiverPeerKey::new(recipient_chain, peer, chain)).0,
    }
}

/// One `submit_vaas` transfer and the PDAs its handler expects.
#[derive(Clone)]
pub struct VaaScenario {
    pub vaa: SignedVaa,
    pub sequence: u64,
    pub noreplay_bucket: Pubkey,
    pub source_balance: Pubkey,
    pub dest_balance: Pubkey,
    pub relayer_registration_pda: Pubkey,
    pub hub_pda: Pubkey,
    pub peer_src_pda: Pubkey,
    pub peer_dst_pda: Pubkey,
}

impl VaaScenario {
    /// `sender` on `chain` publishes directly.
    pub fn direct(
        sequence: u64,
        chain: u16,
        sender: [u8; 32],
        recipient_chain: u16,
        peer: [u8; 32],
        (hub_chain, hub): (u16, [u8; 32]),
        payload: &[u8],
    ) -> Self {
        let body = direct_body(chain, sender, sequence, payload);
        Self::build(
            sequence,
            chain,
            sender,
            sender,
            recipient_chain,
            peer,
            (hub_chain, hub),
            body,
        )
    }

    /// `relayer` on `chain` publishes a `DeliveryInstruction` from `sender`.
    #[allow(clippy::too_many_arguments)]
    pub fn relayed(
        sequence: u64,
        chain: u16,
        relayer: [u8; 32],
        sender: [u8; 32],
        recipient_chain: u16,
        peer: [u8; 32],
        (hub_chain, hub): (u16, [u8; 32]),
        payload: &[u8],
    ) -> Self {
        let body = relayed_body(chain, relayer, sequence, sender, payload);
        Self::build(
            sequence,
            chain,
            relayer,
            sender,
            recipient_chain,
            peer,
            (hub_chain, hub),
            body,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn build(
        sequence: u64,
        chain: u16,
        emitter: [u8; 32],
        sender: [u8; 32],
        recipient_chain: u16,
        peer: [u8; 32],
        (hub_chain, hub): (u16, [u8; 32]),
        body: Vec<u8>,
    ) -> Self {
        let RoutePdas {
            noreplay_bucket,
            source_balance,
            dest_balance,
            relayer_registration_pda,
            hub_pda,
            peer_src_pda,
            peer_dst_pda,
        } = route_pdas(
            chain,
            emitter,
            sequence,
            sender,
            recipient_chain,
            peer,
            (hub_chain, hub),
        );
        Self {
            vaa: SignedVaa::new(body),
            sequence,
            noreplay_bucket,
            source_balance,
            dest_balance,
            relayer_registration_pda,
            hub_pda,
            peer_src_pda,
            peer_dst_pda,
        }
    }

    pub fn metas(&self) -> Vec<AccountMeta> {
        vec![
            AccountMeta::new(self.noreplay_bucket, false),
            AccountMeta::new_readonly(noreplay_program_id(), false),
            AccountMeta::new_readonly(noreplay_authority_pda(&program_id()), false),
            AccountMeta::new(self.source_balance, false),
            AccountMeta::new(self.dest_balance, false),
            AccountMeta::new_readonly(system_program_id(), false),
            AccountMeta::new_readonly(self.relayer_registration_pda, false),
            AccountMeta::new_readonly(self.hub_pda, false),
            AccountMeta::new_readonly(self.peer_src_pda, false),
            AccountMeta::new_readonly(self.peer_dst_pda, false),
        ]
    }

    pub fn keyed(&self, accounts: VaaAccounts) -> Vec<(Pubkey, Account)> {
        vec![
            (self.noreplay_bucket, accounts.bucket),
            keyed_account_for_noreplay_program(),
            (
                noreplay_authority_pda(&program_id()),
                system_owned_account(0),
            ),
            (self.source_balance, accounts.source_balance),
            (self.dest_balance, accounts.dest_balance),
            keyed_account_for_system_program(),
            (self.relayer_registration_pda, accounts.relayer_registration),
            (self.hub_pda, accounts.hub),
            (self.peer_src_pda, accounts.peer_src),
            (self.peer_dst_pda, accounts.peer_dst),
        ]
    }

    pub fn submit(&self, mollusk: &Mollusk, accounts: VaaAccounts) -> InstructionResult {
        let ix_data = submit_vaas_ix_data(self.vaa.guardian_set_bump, &self.vaa.body);
        self.submit_with(mollusk, accounts, ix_data)
    }

    pub fn submit_with(
        &self,
        mollusk: &Mollusk,
        accounts: VaaAccounts,
        ix_data: Vec<u8>,
    ) -> InstructionResult {
        self.vaa.submit(
            mollusk,
            program_id(),
            &ix_data,
            self.metas(),
            self.keyed(accounts),
        )
    }
}

/// Input state for the handler-owned slots.
#[derive(Clone)]
pub struct VaaAccounts {
    pub bucket: Account,
    pub source_balance: Account,
    pub dest_balance: Account,
    pub relayer_registration: Account,
    pub hub: Account,
    pub peer_src: Account,
    pub peer_dst: Account,
}

impl VaaAccounts {
    /// `sender` on `chain` under `hub`, cross-registered with `peer` on `recipient_chain`;
    /// balances and relayer registration absent.
    pub fn registered(
        chain: u16,
        sender: [u8; 32],
        recipient_chain: u16,
        peer: [u8; 32],
        (hub_chain, hub): (u16, [u8; 32]),
    ) -> Self {
        Self {
            bucket: noreplay_bucket_unmarked(),
            source_balance: uninitialised_pda_account(),
            dest_balance: uninitialised_pda_account(),
            relayer_registration: uninitialised_pda_account(),
            hub: hub_account(&hub_layout(chain, sender, hub_chain, hub)),
            peer_src: peer_account(&peer_layout(chain, sender, recipient_chain, peer)),
            peer_dst: peer_account(&peer_layout(recipient_chain, peer, chain, sender)),
        }
    }
}

/// The fields of one NTT observation, as the guardian node signs them.
#[derive(Clone, Copy, Debug)]
pub struct Observation {
    pub chain: u16,
    pub emitter: [u8; 32],
    pub sequence: u64,
    pub sender: [u8; 32],
    pub recipient_chain: u16,
    pub trimmed_decimals: u8,
    pub trimmed_amount: u64,
    /// `double_keccak256` of the VAA body.
    pub digest: [u8; 32],
}

impl Observation {
    /// `sender` on `chain` published directly.
    pub fn direct(
        chain: u16,
        sender: [u8; 32],
        sequence: u64,
        recipient_chain: u16,
        trimmed_decimals: u8,
        trimmed_amount: u64,
    ) -> Self {
        let payload = transfer_payload(trimmed_decimals, trimmed_amount, recipient_chain);
        Self {
            chain,
            emitter: sender,
            sequence,
            sender,
            recipient_chain,
            trimmed_decimals,
            trimmed_amount,
            digest: double_keccak256(&direct_body(chain, sender, sequence, &payload)),
        }
    }

    /// `relayer` on `chain` published a `DeliveryInstruction` from `sender`.
    pub fn relayed(
        chain: u16,
        relayer: [u8; 32],
        sender: [u8; 32],
        sequence: u64,
        recipient_chain: u16,
        trimmed_decimals: u8,
        trimmed_amount: u64,
    ) -> Self {
        let payload = transfer_payload(trimmed_decimals, trimmed_amount, recipient_chain);
        Self {
            chain,
            emitter: relayer,
            sequence,
            sender,
            recipient_chain,
            trimmed_decimals,
            trimmed_amount,
            digest: double_keccak256(&relayed_body(chain, relayer, sequence, sender, &payload)),
        }
    }

    pub fn is_relayed(&self) -> bool {
        self.emitter != self.sender
    }

    /// The instruction struct a guardian sends for this observation.
    pub fn ix(
        &self,
        guardian_set_index: u32,
        guardian_index: u8,
        signature: [u8; 65],
        tx_hash: [u8; 32],
    ) -> NttSubmitObservationsIxData {
        NttSubmitObservationsIxData {
            guardian_set_index: guardian_set_index.to_le_bytes(),
            guardian_index,
            signature,
            tx_hash,
            chain: self.chain.to_be_bytes(),
            emitter: self.emitter,
            sequence: self.sequence.to_be_bytes(),
            sender: self.sender,
            recipient_chain: self.recipient_chain.to_be_bytes(),
            trimmed_decimals: self.trimmed_decimals,
            trimmed_amount: self.trimmed_amount.to_be_bytes(),
            digest: self.digest,
        }
    }

    fn digests_with(&self, prefix: &[u8], tx_hash: &[u8; 32]) -> ObservationDigests {
        let ix = self.ix(0, 0, [0; 65], *tx_hash);
        observation_digests(prefix, tx_hash, &ix.fields_and_digest())
    }

    pub fn content_digest(&self) -> [u8; 32] {
        self.digests_with(NTT_SUBMIT_OBSERVATION_PREFIX, &TX_HASH)
            .content
    }

    pub fn signing_digest(&self) -> [u8; 32] {
        self.signing_digest_with(NTT_SUBMIT_OBSERVATION_PREFIX, &TX_HASH)
    }

    pub fn signing_digest_with(&self, prefix: &[u8], tx_hash: &[u8; 32]) -> [u8; 32] {
        self.digests_with(prefix, tx_hash).signing
    }
}

/// One observation flow: the guardians, and the PDAs `submit_observations` expects.
#[derive(Clone)]
pub struct ObsScenario {
    pub obs: Observation,
    pub hub: (u16, [u8; 32]),
    pub peer: [u8; 32],
    pub guardian_set_index: u32,
    pub guardians: Vec<Guardian>,
    pub pending_pda: Pubkey,
    pub guardian_set: Pubkey,
    pub noreplay_bucket: Pubkey,
    pub source_balance: Pubkey,
    pub dest_balance: Pubkey,
    pub relayer_registration_pda: Pubkey,
    pub hub_pda: Pubkey,
    pub peer_src_pda: Pubkey,
    pub peer_dst_pda: Pubkey,
}

impl ObsScenario {
    /// `obs.sender` under `hub`, cross-registered with `peer` on `obs.recipient_chain`.
    pub fn new(obs: Observation, peer: [u8; 32], hub: (u16, [u8; 32])) -> Self {
        Self::with_guardians(
            obs,
            peer,
            hub,
            GUARDIAN_SET_INDEX,
            make_guardians(GUARDIAN_COUNT, 0x42),
        )
    }

    pub fn with_guardians(
        obs: Observation,
        peer: [u8; 32],
        hub: (u16, [u8; 32]),
        guardian_set_index: u32,
        guardians: Vec<Guardian>,
    ) -> Self {
        let RoutePdas {
            noreplay_bucket,
            source_balance,
            dest_balance,
            relayer_registration_pda,
            hub_pda,
            peer_src_pda,
            peer_dst_pda,
        } = route_pdas(
            obs.chain,
            obs.emitter,
            obs.sequence,
            obs.sender,
            obs.recipient_chain,
            peer,
            hub,
        );
        Self {
            obs,
            hub,
            peer,
            guardian_set_index,
            guardians,
            pending_pda: derive_pending_pda(
                &program_id(),
                obs.chain,
                &obs.emitter,
                obs.sequence,
                guardian_set_index,
                &obs.content_digest(),
            )
            .0,
            guardian_set: derive_guardian_set_pda(guardian_set_index, &core_bridge_program_id()).0,
            noreplay_bucket,
            source_balance,
            dest_balance,
            relayer_registration_pda,
            hub_pda,
            peer_src_pda,
            peer_dst_pda,
        }
    }

    pub fn account_metas(&self) -> Vec<AccountMeta> {
        self.account_metas_for(SUBMITTER)
    }

    /// `submitter` signs and is the rent recipient.
    pub fn account_metas_for(&self, submitter: Pubkey) -> Vec<AccountMeta> {
        vec![
            AccountMeta::new(submitter, true),
            AccountMeta::new(self.pending_pda, false),
            AccountMeta::new_readonly(self.guardian_set, false),
            AccountMeta::new(self.noreplay_bucket, false),
            AccountMeta::new_readonly(system_program_id(), false),
            AccountMeta::new_readonly(noreplay_program_id(), false),
            AccountMeta::new_readonly(noreplay_authority_pda(&program_id()), false),
            AccountMeta::new(self.source_balance, false),
            AccountMeta::new(self.dest_balance, false),
            AccountMeta::new(submitter, false),
            AccountMeta::new_readonly(self.relayer_registration_pda, false),
            AccountMeta::new_readonly(self.hub_pda, false),
            AccountMeta::new_readonly(self.peer_src_pda, false),
            AccountMeta::new_readonly(self.peer_dst_pda, false),
        ]
    }

    /// Hub and peers registered, relayer registered when the observation is relayed,
    /// balances absent, pending absent.
    pub fn initial_accounts(&self) -> Vec<(Pubkey, Account)> {
        let (chain, sender, recipient_chain) =
            (self.obs.chain, self.obs.sender, self.obs.recipient_chain);
        let relayer_registration = if self.obs.is_relayed() {
            chain_registration_account_for(&program_id(), chain, self.obs.emitter)
        } else {
            uninitialised_pda_account()
        };
        vec![
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
            (
                noreplay_authority_pda(&program_id()),
                system_owned_account(0),
            ),
            (self.source_balance, uninitialised_pda_account()),
            (self.dest_balance, uninitialised_pda_account()),
            (self.relayer_registration_pda, relayer_registration),
            (
                self.hub_pda,
                hub_account(&hub_layout(chain, sender, self.hub.0, self.hub.1)),
            ),
            (
                self.peer_src_pda,
                peer_account(&peer_layout(chain, sender, recipient_chain, self.peer)),
            ),
            (
                self.peer_dst_pda,
                peer_account(&peer_layout(recipient_chain, self.peer, chain, sender)),
            ),
        ]
    }

    pub fn ix_data(&self, guardian_index: u8) -> Vec<u8> {
        let signature = sign_digest(
            &self.guardians[guardian_index as usize],
            &self.obs.signing_digest(),
        );
        self.ix_data_signed(guardian_index, signature, &TX_HASH)
    }

    pub fn ix_data_signed(
        &self,
        guardian_index: u8,
        signature: [u8; 65],
        tx_hash: &[u8; 32],
    ) -> Vec<u8> {
        let ix = self
            .obs
            .ix(self.guardian_set_index, guardian_index, signature, *tx_hash);
        wire::framed(
            NttInstruction::SubmitObservations as u8,
            bytemuck::bytes_of(&ix),
            &[],
        )
    }

    pub fn submit_with(
        &self,
        mollusk: &Mollusk,
        accounts: Vec<(Pubkey, Account)>,
        ix_data: Vec<u8>,
        metas: Vec<AccountMeta>,
    ) -> InstructionResult {
        let ix = Instruction::new_with_bytes(program_id(), &ix_data, metas);
        mollusk.process_instruction(&ix, &accounts)
    }

    pub fn submit_once(
        &self,
        mollusk: &Mollusk,
        accounts: Vec<(Pubkey, Account)>,
        guardian_index: u8,
    ) -> InstructionResult {
        self.submit_with(
            mollusk,
            accounts,
            self.ix_data(guardian_index),
            self.account_metas(),
        )
    }

    pub fn submit_n(&self, mollusk: &Mollusk, n: u8) -> Vec<(Pubkey, Account)> {
        self.submit_range(mollusk, self.initial_accounts(), 0..n)
    }

    pub fn submit_range(
        &self,
        mollusk: &Mollusk,
        accounts: Vec<(Pubkey, Account)>,
        range: std::ops::Range<u8>,
    ) -> Vec<(Pubkey, Account)> {
        accountant_test_harness::submit_range(mollusk, accounts, range, |m, a, i| {
            self.submit_once(m, a, i)
        })
    }
}
