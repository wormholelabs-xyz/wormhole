//! `submit_observations` against the real NoReplay program: 13 test guardians
//! reach quorum on an attest body, the commit log fires once, the NoReplay bit
//! is set, and a 14th observation rejects with `AlreadyAccounted`.

use accountant_operational_core::accounts::chain_registration;
use accountant_operational_core::cpi::noreplay::derive_bucket_pda;
use accountant_operational_core::support::quorum::derive_pending_pda;
use global_accountant_definitions::GlobalAccountantError;
use solana_instruction::{AccountMeta, Instruction};
use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use solana_signer::Signer;

use crate::common::{
    assert_bucket_marked, attest_body, chain_registration_account, core_bridge_program_id,
    derive_guardian_set_pda, double_keccak256, emitter, guardian_keys, guardian_set_account,
    make_guardians, noreplay_authority_pda, sign_digest, signing_digest,
    submit_observations_ix_data, system_program_id, Guardian, GUARDIAN_COUNT, GUARDIAN_SET_INDEX,
    NOREPLAY_PROGRAM_ID, QUORUM,
};
use crate::harness::{
    assert_canonical_log_in_tx, deploy_programs, fund, send, send_expect_error, set_account,
    start_surfpool, ProgramImage, SurfpoolOptions,
};

const CHAIN: u16 = 2;
const SEQUENCE: u64 = 0x42;
const SUBMITTER_LAMPORTS: u64 = 20_000_000_000;

/// Accounts one observation touches. Fixed for the whole test; only the signer varies.
struct Observation {
    program_id: Pubkey,
    submitter: Pubkey,
    pending: Pubkey,
    guardian_set: Pubkey,
    bucket: Pubkey,
    noreplay_authority: Pubkey,
    chain_registration: Pubkey,
    body: Vec<u8>,
}

impl Observation {
    fn ix(&self, guardian: &Guardian, guardian_index: u8) -> Instruction {
        let signature = sign_digest(guardian, &signing_digest(&self.body));
        // Attest body: the balance slots are unused, so the authority PDA fills them.
        Instruction {
            program_id: self.program_id,
            accounts: vec![
                AccountMeta::new(self.submitter, true),
                AccountMeta::new(self.pending, false),
                AccountMeta::new_readonly(self.guardian_set, false),
                AccountMeta::new(self.bucket, false),
                AccountMeta::new_readonly(system_program_id(), false),
                AccountMeta::new_readonly(NOREPLAY_PROGRAM_ID, false),
                AccountMeta::new_readonly(self.noreplay_authority, false),
                AccountMeta::new(self.noreplay_authority, false),
                AccountMeta::new(self.noreplay_authority, false),
                AccountMeta::new(self.submitter, false),
                AccountMeta::new_readonly(self.chain_registration, false),
            ],
            data: submit_observations_ix_data(
                GUARDIAN_SET_INDEX,
                guardian_index,
                signature,
                &self.body,
            ),
        }
    }
}

#[test]
#[ignore = "spawns surfpool subprocess; run via `just e2e`"]
fn surfpool_submit_observations_real_noreplay() {
    let guard = start_surfpool(SurfpoolOptions::offline("ga-surfpool-submit-observations"));
    let rpc = guard.rpc_client();

    let accountant = ProgramImage::accountant();
    let program_id = accountant.program_id;
    deploy_programs(&rpc, &[accountant, ProgramImage::noreplay()]);

    let submitter = Keypair::new();
    fund(&rpc, &submitter.pubkey(), SUBMITTER_LAMPORTS);

    let emitter = emitter(0);
    let body = attest_body(CHAIN, emitter, SEQUENCE);
    let digest = double_keccak256(&body);

    let guardians = make_guardians(GUARDIAN_COUNT, 0x42);
    let (guardian_set, _) = derive_guardian_set_pda(GUARDIAN_SET_INDEX, &core_bridge_program_id());
    set_account(
        &rpc,
        &guardian_set,
        &guardian_set_account(
            GUARDIAN_SET_INDEX,
            &guardian_keys(&guardians),
            0,
            0,
            &core_bridge_program_id(),
        ),
    );

    let (chain_registration_pda, _) = chain_registration::derive_pda(&program_id, CHAIN);
    set_account(
        &rpc,
        &chain_registration_pda,
        &chain_registration_account(CHAIN, emitter),
    );

    let noreplay_authority = noreplay_authority_pda(&program_id);
    let observation = Observation {
        program_id,
        submitter: submitter.pubkey(),
        pending: derive_pending_pda(
            &program_id,
            CHAIN,
            &emitter,
            SEQUENCE,
            GUARDIAN_SET_INDEX,
            &digest,
        )
        .0,
        guardian_set,
        bucket: derive_bucket_pda(&noreplay_authority, CHAIN, &emitter, SEQUENCE).0,
        noreplay_authority,
        chain_registration: chain_registration_pda,
        body,
    };

    let mut quorum_sig = None;
    for index in 0..QUORUM {
        let ix = observation.ix(&guardians[index as usize], index);
        quorum_sig = Some(send(
            &rpc,
            &format!("submit_observations[{index}]"),
            &[ix],
            &[&submitter],
        ));
    }
    let quorum_sig = quorum_sig.expect("QUORUM > 0");

    assert_canonical_log_in_tx(
        &rpc,
        &quorum_sig,
        CHAIN,
        &emitter,
        SEQUENCE,
        &digest,
        GUARDIAN_SET_INDEX,
    );
    let bucket = rpc
        .get_account(&observation.bucket)
        .expect("bucket PDA exists post-quorum");
    assert_bucket_marked(&bucket, SEQUENCE);

    let extra = observation.ix(&guardians[QUORUM as usize], QUORUM);
    send_expect_error(
        &rpc,
        "submit_observations after quorum",
        &[extra],
        &[&submitter],
        GlobalAccountantError::AlreadyAccounted,
    );
}
