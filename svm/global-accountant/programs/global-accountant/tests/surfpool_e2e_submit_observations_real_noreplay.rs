//! Surfpool E2E — `submit_observations` driven against the **real**
//! `solana-noreplay` program co-deployed alongside global-accountant.
//!
//! Regression guard for the real CPI to `solana-noreplay`'s `MarkUsed` from
//! inside the quorum-completing branch of `submit_observations`. Deploys the
//! **production-shape** `.so` (built with `make build-prod`, no mock
//! features), wires a fresh `noreplay-authority` PDA owned by
//! global-accountant, and drives 13 distinct guardian observations until the
//! 13th triggers the CPI and flips the real bitmap bit.
//!
//! # Run
//!
//! ```sh
//! # From svm/global-accountant/
//! make test-e2e-submit-obs
//! ```
//!
//! Requires `solana_noreplay.so` at
//! `~/WormholeLabs/CoreTeam/solana-noreplay/target/deploy/solana_noreplay.so`
//! (same source as the noreplay-smoke test) and an up-to-date production
//! build of `global_accountant.so`. The Makefile target ensures both before
//! invoking `cargo test`.

#![allow(clippy::too_many_arguments)]

use std::time::Duration;

use global_accountant_definitions::{
    Instruction as IxDiscriminator, NOREPLAY_AUTHORITY_SEED_PREFIX,
    DIGEST_SEED_PREFIX, PENDING_SEED_PREFIX,
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
    await_confirmed, deploy_program, derive_noreplay_bitmap_pda, noreplay_so_path, so_path,
    start_surfpool, SurfpoolOptions, NOREPLAY_PROGRAM_ID,
};

/// `BITS_PER_BUCKET` mirror from `solana-noreplay::state` — must match the
/// on-chain constant or our bucket-index arithmetic drifts from the program's.
const BITS_PER_BUCKET: u64 = 1024;

/// Account-data offset of the bitmap inside a 129-byte noreplay PDA. Byte 0 is
/// the stored bump; bytes 1..129 are the 128-byte bitmap. Per
/// `solana_noreplay::state::BitmapAccount::from_slice`.
const NOREPLAY_BITMAP_OFFSET: usize = 1;

// ============================================================================
// Guardian fixture
// ============================================================================

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
        out.push(Guardian { secret, eth_address });
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

/// Build a Core-Bridge-style `GuardianSet` account payload matching
/// `wormhole_svm_definitions::zero_copy::GuardianSet`. Owner is set via the
/// cheatcode; here we just emit the data buffer.
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

// ============================================================================
// PDA derivation mirrors of the on-chain helpers
// ============================================================================

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
        &[PENDING_SEED_PREFIX, &chain_be, emitter, &sequence_be, digest],
        program_id,
    )
}

fn derive_digest_pda(
    program_id: &Pubkey,
    chain: u16,
    emitter: &[u8; 32],
    sequence: u64,
) -> (Pubkey, u8) {
    let chain_be = chain.to_be_bytes();
    let sequence_be = sequence.to_be_bytes();
    Pubkey::find_program_address(
        &[DIGEST_SEED_PREFIX, &chain_be, emitter, &sequence_be],
        program_id,
    )
}

/// Derive the noreplay-authority PDA owned by global-accountant. Mirrors the
/// on-chain `derive_noreplay_authority` helper.
fn derive_noreplay_authority_pda(program_id: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[NOREPLAY_AUTHORITY_SEED_PREFIX], program_id)
}

/// Build the 34-byte noreplay namespace `(chain_be ‖ emitter)` matching the
/// on-chain derivation. The big-endian chain choice mirrors the VAA wire
/// format (and the `DIGEST_SEED_PREFIX` layout in `open_digest`); the Phase
/// 2.2.2 smoke test used LE because it was deriving on the client side only,
/// before the production layout was locked.
fn build_namespace(chain: u16, emitter: &[u8; 32]) -> [u8; 34] {
    let mut ns = [0u8; 34];
    ns[..2].copy_from_slice(&chain.to_be_bytes());
    ns[2..].copy_from_slice(emitter);
    ns
}

// ============================================================================
// Instruction builders
// ============================================================================

fn submit_observations_ix_data(
    chain: u16,
    emitter: &[u8; 32],
    sequence: u64,
    digest: &[u8; 32],
    guardian_set_index: u32,
    guardian_index: u8,
    signature: &[u8; 65],
    pending_bump: u8,
    digest_bump: u8,
    body: &[u8],
) -> Vec<u8> {
    // Wire shape: 1-byte discriminator + 146-byte fixed prefix + 2-byte body
    // length (LE) + body bytes. The program re-derives
    // `keccak256(keccak256(body)) == digest` and parses the Token Bridge
    // payload for balance work.
    let mut data = Vec::with_capacity(1 + 146 + 2 + body.len());
    data.push(IxDiscriminator::SubmitObservations as u8);
    data.extend_from_slice(&chain.to_be_bytes());
    data.extend_from_slice(emitter);
    data.extend_from_slice(&sequence.to_be_bytes());
    data.extend_from_slice(digest);
    data.extend_from_slice(&guardian_set_index.to_le_bytes());
    data.push(guardian_index);
    data.extend_from_slice(signature);
    data.push(pending_bump);
    data.push(digest_bump);
    data.extend_from_slice(&(body.len() as u16).to_le_bytes());
    data.extend_from_slice(body);
    data
}

#[allow(dead_code)] // Reserved for the close_pending real-noreplay sub-test
                    // (currently the AlreadyAccounted assertion exercises the
                    // pre-check rather than the cleanup half — extend this
                    // helper into a close_pending invocation once the e2e
                    // surface needs both halves wired together).
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
    digest_pda: &Pubkey,
    noreplay_authority: &Pubkey,
    chain: u16,
    emitter: &[u8; 32],
    sequence: u64,
    digest: &[u8; 32],
    gsi: u32,
    guardian_index: u8,
    signature: &[u8; 65],
    pending_bump: u8,
    digest_bump: u8,
    body: &[u8],
) -> Instruction {
    // The account list carries two trailing slots: source-chain Account PDA
    // and destination-chain Account PDA. For Attest / Other payloads (no
    // balance work) the program never touches these slots, so re-using the
    // noreplay-authority PDA as a sentinel satisfies the runtime's
    // account-meta declaration without standing up real Account PDAs.
    Instruction {
        program_id: *program_id,
        accounts: vec![
            AccountMeta::new(*submitter, true),
            AccountMeta::new(*pending_pda, false),
            AccountMeta::new_readonly(*guardian_set, false),
            AccountMeta::new(*bitmap_pda, false),
            AccountMeta::new(*digest_pda, false),
            AccountMeta::new_readonly(system_program::ID, false),
            AccountMeta::new_readonly(NOREPLAY_PROGRAM_ID, false),
            AccountMeta::new_readonly(*noreplay_authority, false),
            AccountMeta::new(*noreplay_authority, false), // sentinel source slot
            AccountMeta::new(*noreplay_authority, false), // sentinel dest slot
        ],
        data: submit_observations_ix_data(
            chain,
            emitter,
            sequence,
            digest,
            gsi,
            guardian_index,
            signature,
            pending_bump,
            digest_bump,
            body,
        ),
    }
}

// ============================================================================
// Test
// ============================================================================

#[test]
#[ignore = "spawns surfpool subprocess; run via `make test-e2e-submit-obs` or `cargo test -- --ignored`"]
fn surfpool_submit_observations_real_noreplay() {
    // ----- Step 1: locate both .so artifacts before spending boot time. -----
    let ga_so = so_path("global_accountant");
    let ga_bytes = std::fs::read(&ga_so).unwrap_or_else(|e| {
        panic!(
            "could not read {}: {e}. Run `make build-prod` first.",
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

    // ----- Step 2: boot surfpool offline. -----
    let guard =
        start_surfpool(SurfpoolOptions::offline("ga-surfpool-submit-real-noreplay"));
    let rpc_url = guard.rpc_url();
    let rpc = guard.rpc_client();

    // Fresh program ID per run so PDAs never collide across test invocations.
    let ga_program_kp = Keypair::new();
    let ga_program_id = ga_program_kp.pubkey();
    eprintln!(
        "[real-cpi] global_accountant program_id={ga_program_id} (fresh) \
         noreplay program_id={NOREPLAY_PROGRAM_ID} (canonical)"
    );

    // ----- Step 3: airdrop the submitter. The noreplay-authority PDA is
    // signed via invoke_signed and never holds lamports itself.
    let submitter = Keypair::new();
    let sig = rpc
        .request_airdrop(&submitter.pubkey(), 20_000_000_000)
        .expect("airdrop submitter");
    await_confirmed("airdrop submitter", Duration::from_secs(10), || {
        rpc.confirm_transaction(&sig)
    });

    // ----- Step 4: deploy both programs. -----
    deploy_program(&rpc_url, &ga_program_id, &ga_bytes);
    deploy_program(&rpc_url, &NOREPLAY_PROGRAM_ID, &noreplay_bytes);

    // ----- Step 5: derive PDAs. -----
    let chain: u16 = 2;
    let mut emitter = [0u8; 32];
    emitter[31] = 0x77;
    let sequence: u64 = 0x42;
    // Build an Attest body and derive the digest from it via
    // `keccak256(keccak256(body))`. The program re-verifies this relationship
    // before any state mutation, so a synthetic digest unrelated to the body
    // rejects with `BodyDigestMismatch`.
    let body = build_attest_body(chain, &emitter, sequence);
    let digest = double_keccak256_host(&body);

    let (pending_pda, pending_bump) =
        derive_pending_pda(&ga_program_id, chain, &emitter, sequence, &digest);
    let (digest_pda, digest_bump) = derive_digest_pda(&ga_program_id, chain, &emitter, sequence);
    let (noreplay_authority, _noreplay_authority_bump) =
        derive_noreplay_authority_pda(&ga_program_id);
    let namespace = build_namespace(chain, &emitter);
    let (bitmap_pda, bitmap_bump) =
        derive_noreplay_bitmap_pda(&noreplay_authority, &namespace, sequence);
    eprintln!(
        "[real-cpi] pending_pda={pending_pda} digest_pda={digest_pda} \
         noreplay_authority={noreplay_authority} bitmap_pda={bitmap_pda} bump={bitmap_bump}"
    );

    // ----- Step 6: synthesise a 19-guardian set under index 4 and inject the
    // GuardianSet account at a fixed pubkey via the `surfnet_setAccount`
    // cheatcode. No Core Bridge ownership check exists today in our program,
    // so any owner works.
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

    // ----- Step 7: drive 13 observations. -----
    for i in 0..13u8 {
        let g = &guardians[i as usize];
        let signature = sign_digest(g, &digest);
        let ix = build_submit_observations_ix(
            &ga_program_id,
            &submitter.pubkey(),
            &pending_pda,
            &guardian_set_pubkey,
            &bitmap_pda,
            &digest_pda,
            &noreplay_authority,
            chain,
            &emitter,
            sequence,
            &digest,
            4,
            i,
            &signature,
            pending_bump,
            digest_bump,
            &body,
        );
        send_and_confirm(
            &rpc,
            &format!("submit_observations[gi={i}]"),
            &[ix],
            &[&submitter],
        );
    }

    // ----- Step 8: assert the bitmap bit got set in the real noreplay PDA.
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

    // ----- Step 9: a 14th submission for the same sequence must hit the
    // pre-check and fail with AlreadyAccounted (custom error 7).
    let extra_signature = sign_digest(&guardians[13], &digest);
    let extra_ix = build_submit_observations_ix(
        &ga_program_id,
        &submitter.pubkey(),
        &pending_pda,
        &guardian_set_pubkey,
        &bitmap_pda,
        &digest_pda,
        &noreplay_authority,
        chain,
        &emitter,
        sequence,
        &digest,
        4,
        13,
        &extra_signature,
        pending_bump,
        digest_bump,
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
) {
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

/// `keccak256(keccak256(body))` — Wormhole VAA digest convention. Host-side
/// mirror of the on-chain `submit_observations::double_keccak256`.
fn double_keccak256_host(body: &[u8]) -> [u8; 32] {
    let inner = solana_keccak_hasher::hashv(&[body]).to_bytes();
    solana_keccak_hasher::hashv(&[&inner]).to_bytes()
}

/// Build a 52-byte VAA body (51-byte header + 1-byte action 0x02 attest).
/// Attest payloads carry more on the wire but the accountant parser only
/// reads the action byte, so 52 bytes is sufficient.
fn build_attest_body(emitter_chain: u16, emitter_address: &[u8; 32], sequence: u64) -> Vec<u8> {
    let mut body = vec![0u8; 52];
    body[8..10].copy_from_slice(&emitter_chain.to_be_bytes());
    body[10..42].copy_from_slice(emitter_address);
    body[42..50].copy_from_slice(&sequence.to_be_bytes());
    body[51] = 0x02;
    body
}
