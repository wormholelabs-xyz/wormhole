//! Prints golden fixtures for `node/pkg/accountant/solana_layout.go` from the program code paths.
//!
//! Run: `just go-fixtures` (the recipe supplies the compile-time addresses)
//! and copy the hex into `solana_layout_test.go`.

use accountant_operational_core::cpi::noreplay::derive_bucket_pda;
use accountant_operational_core::support::quorum::derive_pending_pda;
use accountant_test_fixtures::{Vaa, MAINNET_OTHER_SEQ2211, MAINNET_TRANSFER_SEQ1395207};
use global_accountant_definitions::{
    AccountantDigestLog, GlobalAccountantError, Instruction, PendingObservationsLayout,
    SubmitObservationsIxData, TxId, VaaBodyHeader, SUBMIT_OBSERVATION_PREFIX,
};
use solana_pubkey::Pubkey;

mod common;
use common::{content_digest, noreplay_authority_pda, observation_ix_from_body};

/// Guardian indices covering every word of the 128-bit bitmap.
const SET_BITS: [u8; 6] = [0, 3, 5, 33, 64, 127];

/// Fields and content digests exclude the tx id, so the real-data vectors use a zero one.
const ZERO_TX_ID: TxId<'static> = TxId::Hash(&[0u8; 32]);

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Discriminator plus `SubmitObservationsIxData`.
fn submit_observations_ix_data(ix: &SubmitObservationsIxData) -> Vec<u8> {
    let mut ix_data = Vec::with_capacity(1 + SubmitObservationsIxData::LEN);
    ix_data.push(Instruction::SubmitObservations as u8);
    ix_data.extend_from_slice(bytemuck::bytes_of(ix));
    assert_eq!(ix_data.len(), 1 + SubmitObservationsIxData::LEN);
    ix_data
}

/// Prints the real-data vectors for one fixture VAA under `label`.
fn print_real_vectors(label: &str, vaa: &Vaa, guardian_set_index: u32) {
    let body = vaa.body();
    let ix = observation_ix_from_body(guardian_set_index, 0, [0u8; 65], ZERO_TX_ID, body);
    let fields = ix.fields_and_digest();
    assert_eq!(fields.len(), 143, "{label} fields");
    let digest = content_digest(body);

    let (header, _payload) = VaaBodyHeader::split(body).expect("fixture has a valid VAA header");
    println!("{label}_BODY: {}", hex(body));
    println!("{label}_CHAIN: {}", header.emitter_chain());
    println!("{label}_EMITTER: {}", hex(&header.emitter_address));
    println!("{label}_SEQUENCE: {}", header.sequence());
    println!("{label}_ACTION: {:#04x}", ix.action);
    println!("{label}_VAA_DIGEST: {}", hex(&ix.digest));
    println!("{label}_FIELDS: {}", hex(&fields));
    println!("{label}_CONTENT_DIGEST: {}", hex(&digest));
}

#[test]
fn print_go_layout_fixtures() {
    // Drift in any of these three lengths invalidates every fixture below.
    assert_eq!(PendingObservationsLayout::LEN, 88);
    assert_eq!(SubmitObservationsIxData::LEN, 278);
    assert_eq!(AccountantDigestLog::LEN, 86);

    // Index-derived bytes make byte-order bugs in the Go decoder visible.
    let program_id_bytes: [u8; 32] = core::array::from_fn(|i| i as u8);
    let program_id = Pubkey::new_from_array(program_id_bytes);
    let emitter: [u8; 32] = core::array::from_fn(|i| 0x40 + i as u8);
    let digest: [u8; 32] = core::array::from_fn(|i| 0x80 + i as u8);
    let payer: [u8; 32] = core::array::from_fn(|i| 0xC0 + i as u8);
    let signature: [u8; 65] = core::array::from_fn(|i| i as u8);
    let hash_tx_id: [u8; 32] = core::array::from_fn(|i| 0xE0 + i as u8);
    let signature_tx_id: [u8; 64] = core::array::from_fn(|i| 0xA0 + i as u8);
    let chain: u16 = 2;
    let sequence: u64 = 100_000;
    let guardian_set_index: u32 = 4;
    let guardian_index: u8 = 3;

    println!("PROGRAM_ID: {}", hex(&program_id_bytes));

    let mut pending = PendingObservationsLayout::new(chain, guardian_set_index, digest, payer);
    for index in SET_BITS {
        pending
            .set_signature(index)
            .expect("index below MAX_GUARDIANS");
    }
    let pending_bytes = bytemuck::bytes_of(&pending);
    assert_eq!(pending_bytes.len(), PendingObservationsLayout::LEN);
    assert_eq!(pending.num_signatures(), SET_BITS.len() as u32);
    println!("PENDING_OBSERVATIONS_ACCOUNT: {}", hex(pending_bytes));

    let (pending_pda, pending_bump) = derive_pending_pda(
        &program_id,
        chain,
        &emitter,
        sequence,
        guardian_set_index,
        &digest,
    );
    println!(
        "PENDING_PDA: {} bump={pending_bump}",
        hex(pending_pda.as_array())
    );

    let noreplay_authority = noreplay_authority_pda(&program_id);
    println!(
        "NOREPLAY_AUTHORITY_PDA: {}",
        hex(noreplay_authority.as_array())
    );

    // Sequences 1023 and 1024 straddle a bucket boundary.
    let boundary_chain: u16 = 56;
    let (bucket_1023, bucket_1023_bump) =
        derive_bucket_pda(&noreplay_authority, boundary_chain, &emitter, 1023);
    let (bucket_1024, bucket_1024_bump) =
        derive_bucket_pda(&noreplay_authority, boundary_chain, &emitter, 1024);
    assert_ne!(
        bucket_1023.as_array(),
        bucket_1024.as_array(),
        "sequence 1023 and 1024 must fall in different buckets"
    );
    println!(
        "NOREPLAY_BUCKET_PDA_SEQ_1023: {} bump={bucket_1023_bump}",
        hex(bucket_1023.as_array())
    );
    println!(
        "NOREPLAY_BUCKET_PDA_SEQ_1024: {} bump={bucket_1024_bump}",
        hex(bucket_1024.as_array())
    );

    let commit_log = AccountantDigestLog::new(chain, emitter, sequence, digest, guardian_set_index);
    assert_eq!(commit_log.as_bytes().len(), AccountantDigestLog::LEN);
    println!("ACCDGST_LOG: {}", hex(commit_log.as_bytes()));

    print_real_vectors("TRANSFER", &MAINNET_TRANSFER_SEQ1395207, 6);
    print_real_vectors("OTHER", &MAINNET_OTHER_SEQ2211, 6);

    // The transfer fields of a non-transfer action stay zero.
    let other_ix =
        observation_ix_from_body(6, 0, [0u8; 65], ZERO_TX_ID, MAINNET_OTHER_SEQ2211.body());
    assert_eq!(other_ix.token_chain, [0u8; 2]);
    assert_eq!(other_ix.token_address, [0u8; 32]);
    assert_eq!(other_ix.recipient_chain, [0u8; 2]);
    assert_eq!(other_ix.amount.0, [0u8; 32]);

    let transfer_body = MAINNET_TRANSFER_SEQ1395207.body();
    let (transfer_header, _) =
        VaaBodyHeader::split(transfer_body).expect("fixture has a valid VAA header");
    let (transfer_pending_pda, transfer_pending_bump) = derive_pending_pda(
        &program_id,
        transfer_header.emitter_chain(),
        &transfer_header.emitter_address,
        transfer_header.sequence(),
        6,
        &content_digest(transfer_body),
    );
    println!(
        "TRANSFER_PENDING_PDA: {} bump={transfer_pending_bump}",
        hex(transfer_pending_pda.as_array())
    );

    let ix = observation_ix_from_body(
        guardian_set_index,
        guardian_index,
        signature,
        TxId::Hash(&hash_tx_id),
        transfer_body,
    );
    println!(
        "SUBMIT_OBSERVATIONS_IX_DATA: {}",
        hex(&submit_observations_ix_data(&ix))
    );
    println!("SUBMIT_OBSERVATIONS_IX_DATA_TX_ID: {}", hex(&hash_tx_id));

    let signature_ix = observation_ix_from_body(
        guardian_set_index,
        guardian_index,
        signature,
        TxId::Signature(&signature_tx_id),
        transfer_body,
    );
    println!(
        "SUBMIT_OBSERVATIONS_SIGNATURE_TX_ID_IX_DATA: {}",
        hex(&submit_observations_ix_data(&signature_ix))
    );
    println!(
        "SUBMIT_OBSERVATIONS_SIGNATURE_TX_ID: {}",
        hex(&signature_tx_id)
    );

    // What each guardian signs for the transfer above: keccak256(prefix ‖ tx_id ‖ fields).
    let signing_digest = accountant_operational_core::hash::observation_signing_digest(
        SUBMIT_OBSERVATION_PREFIX,
        TxId::Hash(&hash_tx_id),
        &ix.fields_and_digest(),
    );
    println!("TRANSFER_SIGNING_DIGEST: {}", hex(&signing_digest));
    let signature_signing_digest = accountant_operational_core::hash::observation_signing_digest(
        SUBMIT_OBSERVATION_PREFIX,
        TxId::Signature(&signature_tx_id),
        &signature_ix.fields_and_digest(),
    );
    println!(
        "TRANSFER_SIGNATURE_TX_ID_SIGNING_DIGEST: {}",
        hex(&signature_signing_digest)
    );

    // Codes the Go submit path branches on.
    for (name, code) in [
        ("PAYER_MISMATCH", GlobalAccountantError::PayerMismatch),
        ("ALREADY_ACCOUNTED", GlobalAccountantError::AlreadyAccounted),
        ("INVALID_SIGNATURE", GlobalAccountantError::InvalidSignature),
        (
            "INVALID_GUARDIAN_INDEX",
            GlobalAccountantError::InvalidGuardianIndex,
        ),
        ("ALREADY_SIGNED", GlobalAccountantError::AlreadySigned),
        (
            "EXPIRED_GUARDIAN_SET",
            GlobalAccountantError::ExpiredGuardianSet,
        ),
        (
            "MISSING_CHAIN_REGISTRATION",
            GlobalAccountantError::MissingChainRegistration,
        ),
        (
            "UNREGISTERED_EMITTER",
            GlobalAccountantError::UnregisteredEmitter,
        ),
    ] {
        println!("ERR_{name}: {}", u32::from(code));
    }
}
