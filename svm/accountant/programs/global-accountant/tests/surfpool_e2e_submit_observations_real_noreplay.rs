//! Surfpool E2E — `submit_observations` against the real `solana-noreplay`
//! co-deployed with global-accountant. Drives 13 guardian observations so the
//! quorum-completing branch CPIs into `MarkUsed` and flips the real bitmap bit.
//!
//! # Run
//!
//! ```sh
//! just test-e2e-submit-obs
//! ```
//!
//! `#[ignore]` (spawns surfpool). Requires `solana_noreplay.so` and an
//! up-to-date production `global_accountant.so`; the Makefile target ensures both.

#![allow(clippy::too_many_arguments)]

use std::time::Duration;

use global_accountant_definitions::{
    ChainRegistrationLayout, Instruction as IxDiscriminator, CHAIN_REGISTRATION_SEED_PREFIX,
    NOREPLAY_AUTHORITY_SEED_PREFIX, PENDING_OBSERVATIONS_SEED_PREFIX,
};
use libsecp256k1::{sign, Message, PublicKey, SecretKey};
use solana_client::rpc_config::RpcSendTransactionConfig;
use solana_instruction::{AccountMeta, Instruction};
use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use solana_signer::Signer;
use solana_system_interface::program as system_program;
use solana_transaction::Transaction;

mod common;
use common::{
    assert_canonical_log_in_tx, await_confirmed, deploy_program, derive_noreplay_bitmap_pda,
    noreplay_so_path, so_path, start_surfpool, SurfpoolOptions, NOREPLAY_PROGRAM_ID,
};

/// `BITS_PER_BUCKET` mirror from `solana-noreplay::state`.
const BITS_PER_BUCKET: u64 = 1024;

/// Bitmap offset inside a 129-byte noreplay PDA (byte 0 = bump, 1..129 = bitmap).
const NOREPLAY_BITMAP_OFFSET: usize = 1;

// Guardian fixture.

#[derive(Clone)]
struct Guardian {
    secret: SecretKey,
    eth_address: [u8; 20],
}

fn make_guardians(count: usize, seed: u8) -> Vec<Guardian> {
    let mut out = Vec::with_capacity(count);
    for i in 0..count {
        let mut sk_bytes = [0u8; 32];
        sk_bytes[0] = seed;
        sk_bytes[1] = i as u8;
        sk_bytes[31] = (i as u8).wrapping_add(1);
        let secret =
            SecretKey::parse(&sk_bytes).expect("deterministic seed inside secp256k1 group order");
        let public = PublicKey::from_secret_key(&secret);
        let pk_uncompressed = public.serialize();
        let raw = &pk_uncompressed[1..];
        let hash = solana_keccak_hasher::hashv(&[raw]).to_bytes();
        let mut eth_address = [0u8; 20];
        eth_address.copy_from_slice(&hash[12..]);
        out.push(Guardian {
            secret,
            eth_address,
        });
    }
    out
}

fn sign_digest(guardian: &Guardian, digest: &[u8; 32]) -> [u8; 65] {
    let msg = Message::parse(digest);
    let (sig, rec) = sign(&msg, &guardian.secret);
    let sig_bytes = sig.serialize();
    let mut out = [0u8; 65];
    out[..64].copy_from_slice(&sig_bytes);
    out[64] = rec.serialize();
    out
}

/// Core-Bridge-style `GuardianSet` data buffer (owner is set via the cheatcode).
fn guardian_set_data(
    index: u32,
    keys: &[[u8; 20]],
    creation_time: u32,
    expiration_time: u32,
) -> Vec<u8> {
    let mut data = Vec::with_capacity(8 + keys.len() * 20 + 8);
    data.extend_from_slice(&index.to_le_bytes());
    data.extend_from_slice(&(keys.len() as u32).to_le_bytes());
    for key in keys {
        data.extend_from_slice(key);
    }
    data.extend_from_slice(&creation_time.to_le_bytes());
    data.extend_from_slice(&expiration_time.to_le_bytes());
    data
}

// PDA derivation mirrors of the on-chain helpers.

fn derive_pending_pda(
    program_id: &Pubkey,
    chain: u16,
    emitter: &[u8; 32],
    sequence: u64,
    digest: &[u8; 32],
) -> (Pubkey, u8) {
    let chain_be = chain.to_be_bytes();
    let sequence_be = sequence.to_be_bytes();
    Pubkey::find_program_address(
        &[
            PENDING_OBSERVATIONS_SEED_PREFIX,
            &chain_be,
            emitter,
            &sequence_be,
            digest,
        ],
        program_id,
    )
}

/// Derive the global-accountant noreplay_authority PDA.
fn derive_noreplay_authority_pda(program_id: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[NOREPLAY_AUTHORITY_SEED_PREFIX], program_id)
}

/// Derive the chain-registration PDA for `chain`.
fn derive_chain_registration_pda(program_id: &Pubkey, chain: u16) -> (Pubkey, u8) {
    let chain_be = chain.to_be_bytes();
    Pubkey::find_program_address(&[CHAIN_REGISTRATION_SEED_PREFIX, &chain_be], program_id)
}

/// Build the 34-byte noreplay namespace `(chain_be ‖ emitter)`.
fn build_namespace(chain: u16, emitter: &[u8; 32]) -> [u8; 34] {
    let mut ns = [0u8; 34];
    ns[..2].copy_from_slice(&chain.to_be_bytes());
    ns[2..].copy_from_slice(emitter);
    ns
}

// Instruction builders.

fn submit_observations_ix_data(
    digest: &[u8; 32],
    guardian_set_index: u32,
    guardian_index: u8,
    signature: &[u8; 65],
    body: &[u8],
) -> Vec<u8> {
    // No routing prefix (sourced from the body header) and no bump bytes.
    let mut data = Vec::with_capacity(1 + 102 + 2 + body.len());
    data.push(IxDiscriminator::SubmitObservations as u8);
    data.extend_from_slice(digest);
    data.extend_from_slice(&guardian_set_index.to_le_bytes());
    data.push(guardian_index);
    data.extend_from_slice(signature);
    data.extend_from_slice(&(body.len() as u16).to_le_bytes());
    data.extend_from_slice(body);
    data
}

#[allow(dead_code)] // Reserved for a future close_pending real-noreplay sub-test.
fn close_pending_ix_data(emitter: &[u8; 32], sequence: u64) -> Vec<u8> {
    let mut data = Vec::with_capacity(1 + 32 + 8);
    data.push(IxDiscriminator::ClosePending as u8);
    data.extend_from_slice(emitter);
    data.extend_from_slice(&sequence.to_be_bytes());
    data
}

#[allow(clippy::too_many_arguments)]
fn build_submit_observations_ix(
    program_id: &Pubkey,
    submitter: &Pubkey,
    pending_pda: &Pubkey,
    guardian_set: &Pubkey,
    bitmap_pda: &Pubkey,
    noreplay_authority: &Pubkey,
    chain_registration: &Pubkey,
    digest: &[u8; 32],
    gsi: u32,
    guardian_index: u8,
    signature: &[u8; 65],
    body: &[u8],
) -> Instruction {
    // Production 11-slot account list. Slots 7/8 (source/dest Account PDAs) are
    // only touched on the Transfer branch; for Attest the noreplay_authority PDA
    // is a sentinel. Slot 9 (rent recipient) must equal the recorded payer.
    Instruction {
        program_id: *program_id,
        accounts: vec![
            AccountMeta::new(*submitter, true),
            AccountMeta::new(*pending_pda, false),
            AccountMeta::new_readonly(*guardian_set, false),
            AccountMeta::new(*bitmap_pda, false),
            AccountMeta::new_readonly(system_program::ID, false),
            AccountMeta::new_readonly(NOREPLAY_PROGRAM_ID, false),
            AccountMeta::new_readonly(*noreplay_authority, false),
            AccountMeta::new(*noreplay_authority, false), // sentinel source slot
            AccountMeta::new(*noreplay_authority, false), // sentinel dest slot
            AccountMeta::new(*submitter, false),          // rent recipient
            AccountMeta::new_readonly(*chain_registration, false),
        ],
        data: submit_observations_ix_data(digest, gsi, guardian_index, signature, body),
    }
}

/// 13 observations reach quorum, the real noreplay CPI flips the bitmap bit,
/// and a 14th submission fails with AlreadyAccounted.
#[test]
#[ignore = "spawns surfpool subprocess; run via `just test-e2e-submit-obs` or `cargo test -- --ignored`"]
fn surfpool_submit_observations_real_noreplay() {
    // Locate both .so artifacts.
    let ga_so = so_path("global_accountant");
    let ga_bytes = std::fs::read(&ga_so).unwrap_or_else(|e| {
        panic!(
            "could not read {}: {e}. Run `just build-prod` first.",
            ga_so.display()
        )
    });
    let noreplay_so = noreplay_so_path();
    let noreplay_bytes = std::fs::read(&noreplay_so).unwrap_or_else(|e| {
        panic!(
            "could not read {}: {e}. \
             Rebuild via `cd ~/WormholeLabs/CoreTeam/solana-noreplay && just build` \
             or override with GA_NOREPLAY_SO=<path>.",
            noreplay_so.display()
        )
    });
    eprintln!(
        "[real-cpi] loaded global_accountant.so={} bytes, solana_noreplay.so={} bytes",
        ga_bytes.len(),
        noreplay_bytes.len()
    );

    // Boot surfpool offline.
    let guard = start_surfpool(SurfpoolOptions::offline("ga-surfpool-submit-real-noreplay"));
    let rpc_url = guard.rpc_url();
    let rpc = guard.rpc_client();

    // Fresh program ID per run so PDAs never collide.
    let ga_program_kp = Keypair::new();
    let ga_program_id = ga_program_kp.pubkey();
    eprintln!(
        "[real-cpi] global_accountant program_id={ga_program_id} (fresh) \
         noreplay program_id={NOREPLAY_PROGRAM_ID} (canonical)"
    );

    // Airdrop the submitter (the noreplay_authority PDA never holds lamports).
    let submitter = Keypair::new();
    let sig = rpc
        .request_airdrop(&submitter.pubkey(), 20_000_000_000)
        .expect("airdrop submitter");
    await_confirmed("airdrop submitter", Duration::from_secs(10), || {
        rpc.confirm_transaction(&sig)
    });

    // Deploy both programs.
    deploy_program(&rpc_url, &ga_program_id, &ga_bytes);
    deploy_program(&rpc_url, &NOREPLAY_PROGRAM_ID, &noreplay_bytes);

    // Derive PDAs.
    let chain: u16 = 2;
    let mut emitter = [0u8; 32];
    emitter[31] = 0x77;
    let sequence: u64 = 0x42;
    // Digest must be `keccak256(keccak256(body))` or the program rejects.
    let body = build_attest_body(chain, &emitter, sequence);
    let digest = double_keccak256_host(&body);

    // Only PDA addresses feed the metas; bumps derive on-chain.
    let (pending_pda, _) = derive_pending_pda(&ga_program_id, chain, &emitter, sequence, &digest);
    let (noreplay_authority, _noreplay_authority_bump) =
        derive_noreplay_authority_pda(&ga_program_id);
    let (chain_registration_pda, _) = derive_chain_registration_pda(&ga_program_id, chain);
    let namespace = build_namespace(chain, &emitter);
    let (bitmap_pda, bitmap_bump) =
        derive_noreplay_bitmap_pda(&noreplay_authority, &namespace, sequence);
    eprintln!(
        "[real-cpi] pending_pda={pending_pda} \
         noreplay_authority={noreplay_authority} bitmap_pda={bitmap_pda} bump={bitmap_bump}"
    );

    // Synthesise a 19-guardian set (index 4) and inject it via cheatcode
    // (no Core Bridge ownership check today, so any owner works).
    let guardians = make_guardians(19, 0x42);
    let keys: Vec<[u8; 20]> = guardians.iter().map(|g| g.eth_address).collect();
    let gs_data = guardian_set_data(4, &keys, 0, 0);
    let guardian_set_pubkey = Pubkey::new_unique();
    let gs_owner = Pubkey::new_from_array([0xCCu8; 32]);
    let resp = common::rpc_call(
        &rpc_url,
        "surfnet_setAccount",
        serde_json::json!([
            guardian_set_pubkey.to_string(),
            {
                "lamports": 1_000_000_000u64,
                "owner": gs_owner.to_string(),
                "executable": false,
                "rent_epoch": 0u64,
                "data": common::hex_encode(&gs_data),
            }
        ]),
    );
    assert!(
        resp.get("error").is_none(),
        "surfnet_setAccount failed for guardian set: {resp}"
    );
    let gs_after = rpc
        .get_account(&guardian_set_pubkey)
        .expect("GS account after setAccount");
    assert_eq!(gs_after.data.len(), gs_data.len(), "GS data round-trips");

    // Inject the chain-registration PDA: the program cross-checks the body's
    // (emitter_chain, emitter) against it on every submission. Cheatcode-written
    // since the prod-shape build only creates it via `register_chain` governance.
    let mut registration: ChainRegistrationLayout = bytemuck::Zeroable::zeroed();
    registration.tag = ChainRegistrationLayout::TAG;
    registration.chain = chain;
    registration.emitter_address = emitter;
    let resp = common::rpc_call(
        &rpc_url,
        "surfnet_setAccount",
        serde_json::json!([
            chain_registration_pda.to_string(),
            {
                "lamports": 1_000_000_000u64,
                "owner": ga_program_id.to_string(),
                "executable": false,
                "rent_epoch": 0u64,
                "data": common::hex_encode(bytemuck::bytes_of(&registration)),
            }
        ]),
    );
    assert!(
        resp.get("error").is_none(),
        "surfnet_setAccount failed for chain registration: {resp}"
    );

    // Drive 13 observations. The 13th submission emits the canonical commit
    // log via `sol_log_data`; we capture its tx signature and walk
    // `meta.logMessages` after quorum to verify the on-chain emission shape
    // against [`assert_canonical_log_in_tx`].
    let mut quorum_tx_sig: Option<String> = None;
    for i in 0..13u8 {
        let g = &guardians[i as usize];
        let signature = sign_digest(g, &digest);
        let ix = build_submit_observations_ix(
            &ga_program_id,
            &submitter.pubkey(),
            &pending_pda,
            &guardian_set_pubkey,
            &bitmap_pda,
            &noreplay_authority,
            &chain_registration_pda,
            &digest,
            4,
            i,
            &signature,
            &body,
        );
        let sig = send_and_confirm(
            &rpc,
            &format!("submit_observations[gi={i}]"),
            &[ix],
            &[&submitter],
        );
        if i == 12 {
            quorum_tx_sig = Some(sig);
        }
    }
    let quorum_sig = quorum_tx_sig.expect("13th submission captured a tx signature");
    eprintln!("[real-cpi] quorum-completing tx sig={quorum_sig}");

    // Assert the canonical commit-log payload was emitted on the
    // quorum-completing tx. Off-chain consumers reading `meta.logMessages`
    // and filtering on `ACCOUNTANT_DIGEST_LOG_TAG` is the on-chain breadcrumb
    // that replaced the (removed) DigestAccount PDA.
    assert_canonical_log_in_tx(
        &rpc_url,
        &quorum_sig,
        chain,
        &emitter,
        sequence,
        &digest,
        4, // guardian_set_index used by the quorum-completing observation
    );

    // Assert the bitmap bit got set in the real noreplay PDA.
    let bitmap_after = rpc
        .get_account(&bitmap_pda)
        .expect("bitmap PDA exists post-quorum");
    assert_eq!(
        bitmap_after.owner, NOREPLAY_PROGRAM_ID,
        "bitmap PDA owned by noreplay program"
    );
    assert_eq!(bitmap_after.data.len(), 129, "bitmap PDA is 129 bytes");
    let bit_index = (sequence % BITS_PER_BUCKET) as usize;
    let byte = bitmap_after.data[NOREPLAY_BITMAP_OFFSET + bit_index / 8];
    assert!(
        byte & (1 << (bit_index % 8)) != 0,
        "bit {bit_index} set in the bitmap post-quorum"
    );

    // A 14th submission must fail the pre-check with AlreadyAccounted (0x7).
    let extra_signature = sign_digest(&guardians[13], &digest);
    let extra_ix = build_submit_observations_ix(
        &ga_program_id,
        &submitter.pubkey(),
        &pending_pda,
        &guardian_set_pubkey,
        &bitmap_pda,
        &noreplay_authority,
        &chain_registration_pda,
        &digest,
        4,
        13,
        &extra_signature,
        &body,
    );
    let err = send_expect_failure(
        &rpc,
        "14th submit_observations expected AlreadyAccounted",
        &[extra_ix],
        &[&submitter],
    );
    assert!(
        err.contains("custom program error") && err.contains("0x7"),
        "expected AlreadyAccounted (custom 0x7); got: {err}"
    );

    eprintln!("[real-cpi] all phases green");
}

fn send_and_confirm(
    rpc: &solana_client::rpc_client::RpcClient,
    label: &str,
    ixs: &[Instruction],
    signers: &[&Keypair],
) -> String {
    let blockhash = rpc
        .get_latest_blockhash()
        .unwrap_or_else(|e| panic!("blockhash for {label}: {e}"));
    let tx =
        Transaction::new_signed_with_payer(ixs, Some(&signers[0].pubkey()), signers, blockhash);
    let sig = rpc
        .send_transaction_with_config(
            &tx,
            RpcSendTransactionConfig {
                skip_preflight: true,
                preflight_commitment: None,
                encoding: None,
                max_retries: None,
                min_context_slot: None,
            },
        )
        .unwrap_or_else(|e| panic!("{label} send_transaction: {e}"));
    await_confirmed(label, Duration::from_secs(15), || {
        rpc.confirm_transaction(&sig)
    });
    eprintln!("[real-cpi] {label} tx={sig}");
    sig.to_string()
}

fn send_expect_failure(
    rpc: &solana_client::rpc_client::RpcClient,
    label: &str,
    ixs: &[Instruction],
    signers: &[&Keypair],
) -> String {
    let blockhash = rpc
        .get_latest_blockhash()
        .unwrap_or_else(|e| panic!("blockhash for {label}: {e}"));
    let tx =
        Transaction::new_signed_with_payer(ixs, Some(&signers[0].pubkey()), signers, blockhash);
    match rpc.send_and_confirm_transaction(&tx) {
        Ok(sig) => panic!("{label} unexpectedly confirmed: tx={sig}"),
        Err(e) => e.to_string(),
    }
}

/// `keccak256(keccak256(body))` — Wormhole VAA digest convention, host-side.
fn double_keccak256_host(body: &[u8]) -> [u8; 32] {
    let inner = solana_keccak_hasher::hashv(&[body]).to_bytes();
    solana_keccak_hasher::hashv(&[&inner]).to_bytes()
}

/// 52-byte VAA body (header + action 0x02 attest); the parser only reads the
/// action byte.
fn build_attest_body(emitter_chain: u16, emitter_address: &[u8; 32], sequence: u64) -> Vec<u8> {
    let mut body = vec![0u8; 52];
    body[8..10].copy_from_slice(&emitter_chain.to_be_bytes());
    body[10..42].copy_from_slice(emitter_address);
    body[42..50].copy_from_slice(&sequence.to_be_bytes());
    body[51] = 0x02;
    body
}
