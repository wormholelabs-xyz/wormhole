//! Prints golden fixtures for `node/pkg/accountant/solana_layout.go` from the program code paths.
//!
//! Run: `cargo test -p global-accountant --test go_fixture_vectors -- --nocapture`
//! and copy the hex into `solana_layout_test.go`.

use {
    accountant_operational_core::instructions::{
        noreplay::derive_bucket_pda,
        quorum::{BODY_MIN_LEN, SUBMIT_FIXED_LEN},
    },
    global_accountant_definitions::{
        CommitLog, Instruction, PendingObservationsLayout, NOREPLAY_AUTHORITY_SEED_PREFIX,
        PENDING_OBSERVATIONS_SEED_PREFIX,
    },
    pinocchio::Address,
};

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[test]
fn print_go_layout_fixtures() {
    // Index-derived bytes make byte-order bugs in the Go decoder visible.
    let program_id_bytes: [u8; 32] = core::array::from_fn(|i| i as u8);
    let emitter: [u8; 32] = core::array::from_fn(|i| 0x40 + i as u8);
    let digest: [u8; 32] = core::array::from_fn(|i| 0x80 + i as u8);
    let payer: [u8; 32] = core::array::from_fn(|i| 0xC0 + i as u8);
    let signature: [u8; 65] = core::array::from_fn(|i| i as u8);
    let tx_hash: [u8; 32] = core::array::from_fn(|i| 0xE0 + i as u8);
    let chain: u16 = 2;
    let sequence: u64 = 100_000;
    let guardian_set_index: u32 = 4;
    let guardian_index: u8 = 3;
    let signatures_bitmap: u32 = 0b0010_1001;

    let program_id = Address::from(program_id_bytes);

    let mut pending_layout: PendingObservationsLayout = bytemuck::Zeroable::zeroed();
    pending_layout.tag = PendingObservationsLayout::TAG;
    pending_layout.chain = chain;
    pending_layout.guardian_set_index = guardian_set_index;
    pending_layout.signatures = signatures_bitmap;
    pending_layout.digest = digest;
    pending_layout.payer = payer;
    let pending_account_bytes = bytemuck::bytes_of(&pending_layout);
    assert_eq!(pending_account_bytes.len(), PendingObservationsLayout::LEN);
    println!(
        "PENDING_OBSERVATIONS_ACCOUNT: {}",
        hex(pending_account_bytes)
    );

    // Seeds match quorum.rs create_pending_pda.
    let chain_be = chain.to_be_bytes();
    let sequence_be = sequence.to_be_bytes();
    let (pending_pda, pending_bump) = Address::find_program_address(
        &[
            PENDING_OBSERVATIONS_SEED_PREFIX,
            &chain_be,
            &emitter,
            &sequence_be,
            &digest,
        ],
        &program_id,
    );
    println!(
        "PENDING_PDA: {} bump={pending_bump}",
        hex(pending_pda.as_array())
    );

    // Seeds match noreplay.rs mark_used.
    let (noreplay_authority, authority_bump) =
        Address::find_program_address(&[NOREPLAY_AUTHORITY_SEED_PREFIX], &program_id);
    println!(
        "NOREPLAY_AUTHORITY_PDA: {} bump={authority_bump}",
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

    let commit_log = CommitLog {
        chain,
        emitter,
        sequence,
        digest,
        guardian_set_index,
    };
    println!("ACCDGST_LOG: {}", hex(&commit_log.to_bytes()));

    // Body header offsets follow vaa.rs parse_vaa_namespace_key.
    let mut body = [0u8; BODY_MIN_LEN];
    body[8..10].copy_from_slice(&chain_be);
    body[10..42].copy_from_slice(&emitter);
    body[42..50].copy_from_slice(&sequence_be);
    body[50] = 0x01;

    let mut ix_data = Vec::with_capacity(1 + SUBMIT_FIXED_LEN + tx_hash.len() + 2 + body.len());
    ix_data.push(Instruction::SubmitObservations as u8);
    ix_data.extend_from_slice(&guardian_set_index.to_le_bytes());
    ix_data.push(guardian_index);
    ix_data.extend_from_slice(&signature);
    assert_eq!(ix_data.len(), 1 + SUBMIT_FIXED_LEN);
    ix_data.extend_from_slice(&tx_hash);
    ix_data.extend_from_slice(&(body.len() as u16).to_le_bytes());
    ix_data.extend_from_slice(&body);
    println!("SUBMIT_OBSERVATIONS_IX_DATA: {}", hex(&ix_data));
    println!(
        "SUBMIT_OBSERVATIONS_IX_DATA_TX_HASH_EXPECTED: {}",
        hex(&tx_hash)
    );
}
