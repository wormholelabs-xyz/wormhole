//! Phase 2.2.2 surfpool smoke test for `solana-noreplay` co-deployment.
//!
//! This is the deployment-and-wire-format groundwork for the Phase 2.3 NoReplay
//! CPI from `submit_observations`. It proves we can:
//!
//!   1. Spin up surfpool's in-memory simnet (no mainnet datasource).
//!   2. Deploy our `global-accountant.so` at a fresh program ID.
//!   3. Deploy `solana-noreplay`'s `solana_noreplay.so` at the canonical devnet
//!      ID `repMHgR5BEpGLeZvM5iGoNNDPw4eu2BS6sXJzaC8K4t` (the same address its
//!      compile-time `NOREPLAY_PROGRAM_ID` env var bakes into the binary).
//!   4. Drive `CreateBitmap` + `MarkUsed` (and a replay-rejection MarkUsed)
//!      directly against the noreplay program over JSON-RPC, with a fresh
//!      Keypair acting as the authority. Phase 2.3 will replace that
//!      Keypair-signed authority with a PDA owned by global-accountant invoked
//!      via `invoke_signed`.
//!
//! # Run
//!
//! ```sh
//! # From svm/global-accountant/
//! make test-e2e-noreplay-smoke
//! ```
//!
//! Requires `solana_noreplay.so` at
//! `~/WormholeLabs/CoreTeam/solana-noreplay/target/deploy/solana_noreplay.so`.
//! Rebuild via `cd ~/WormholeLabs/CoreTeam/solana-noreplay && just build`
//! (which runs `cargo build-sbf --manifest-path program/Cargo.toml --no-default-features`).
//!
//! # Why this test is not in `test-e2e`
//!
//! `test-e2e` is the unified surfpool-aggregator target run on every CI build.
//! This smoke test exercises a one-off "can we deploy both programs together"
//! invariant — not a regression-prone code path inside our own program — so
//! adding it to the per-PR CI rotation would burn ~10 seconds on every run for
//! a property that does not depend on our diff. Keep it gated behind its own
//! make target; revisit if Phase 2.3 turns it into a true regression test.

use std::time::Duration;

use solana_client::rpc_config::RpcSendTransactionConfig;
use solana_instruction::{AccountMeta, Instruction};
use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use solana_signer::Signer;
use solana_system_interface::program as system_program;
use solana_transaction::Transaction;

mod common;
use common::{
    await_confirmed, deploy_program, derive_noreplay_bitmap_pda, noreplay_ix_data, so_path,
    start_surfpool, SurfpoolOptions, NOREPLAY_PROGRAM_ID,
};

/// Pinned path to the pre-built `solana_noreplay.so`. The neighbouring
/// CoreTeam repo is git-managed and the `.so` is the canonical deploy artifact;
/// the Makefile target asserts the file exists before running this binary so a
/// missing build surfaces with a clear remediation message.
const NOREPLAY_SO_PATH: &str =
    "/Users/smurf/WormholeLabs/CoreTeam/solana-noreplay/target/deploy/solana_noreplay.so";

/// Discriminators from `solana_noreplay::instruction::{CREATE_BITMAP, MARK_USED}`.
const DISC_CREATE_BITMAP: u8 = 0;
const DISC_MARK_USED: u8 = 1;

/// Total account size (1-byte stored bump + 128-byte bitmap), per
/// `solana_noreplay::state::BITMAP_ACCOUNT_SIZE`.
const BITMAP_ACCOUNT_SIZE: usize = 129;

/// `BITS_PER_BUCKET` from `solana_noreplay::state` — must match the on-chain
/// constant or our bucket-index arithmetic drifts from the program's.
const BITS_PER_BUCKET: u64 = 1024;

/// Build the noreplay namespace for a `(chain, emitter)` pair.
/// Per the README's Wormhole example: `chain_id (u16 LE) || emitter (32 bytes)`.
/// 34-byte namespace; splits into chunk_0 = bytes[0..32], chunk_1 = bytes[32..34]
/// at the program level — verified in `solana_noreplay::pda::BitmapPdaSeeds::new`.
fn build_namespace(chain: u16, emitter: &[u8; 32]) -> [u8; 34] {
    let mut ns = [0u8; 34];
    ns[..2].copy_from_slice(&chain.to_le_bytes());
    ns[2..].copy_from_slice(emitter);
    ns
}

/// Build a noreplay instruction (CreateBitmap, MarkUsed, or UnmarkUsed) ready
/// to be wrapped in a `Transaction`. Accounts shape is identical across all
/// three discriminators; `authority_signs` toggles the signer bit on the
/// authority account (true for MarkUsed/UnmarkUsed, false for CreateBitmap).
fn noreplay_instruction(
    discriminator: u8,
    payer: &Pubkey,
    authority: &Pubkey,
    authority_signs: bool,
    bitmap_pda: &Pubkey,
    namespace: &[u8],
    sequence: u64,
) -> Instruction {
    Instruction {
        program_id: NOREPLAY_PROGRAM_ID,
        accounts: vec![
            AccountMeta::new(*payer, true),
            // CreateBitmap accepts a readonly non-signer authority; MarkUsed and
            // UnmarkUsed require the authority to sign.
            if authority_signs {
                AccountMeta::new_readonly(*authority, true)
            } else {
                AccountMeta::new_readonly(*authority, false)
            },
            AccountMeta::new(*bitmap_pda, false),
            AccountMeta::new_readonly(system_program::ID, false),
        ],
        data: noreplay_ix_data(discriminator, namespace, sequence),
    }
}

#[test]
#[ignore = "spawns surfpool subprocess; run via `make test-e2e-noreplay-smoke` or `cargo test -- --ignored`"]
fn surfpool_noreplay_smoke() {
    // ----- Phase 1: locate both .so artifacts before we spend boot time. -----
    let ga_so = so_path("global_accountant");
    let ga_bytes = std::fs::read(&ga_so).unwrap_or_else(|e| {
        panic!(
            "could not read {}: {e}. Run `make build-dev` first.",
            ga_so.display()
        )
    });
    let noreplay_bytes = std::fs::read(NOREPLAY_SO_PATH).unwrap_or_else(|e| {
        panic!(
            "could not read {NOREPLAY_SO_PATH}: {e}. \
             Rebuild via `cd ~/WormholeLabs/CoreTeam/solana-noreplay && just build`."
        )
    });
    eprintln!(
        "[smoke] loaded global_accountant.so={} bytes, solana_noreplay.so={} bytes",
        ga_bytes.len(),
        noreplay_bytes.len()
    );

    // ----- Phase 2: boot surfpool offline (no mainnet fork needed). -----
    let guard = start_surfpool(SurfpoolOptions::offline("ga-surfpool-noreplay-smoke"));
    let rpc_url = guard.rpc_url();
    let rpc = guard.rpc_client();

    // Fresh program ID for global-accountant so reruns on the same dev machine
    // never collide with stale PDAs. The noreplay program ID is fixed — see the
    // module doc above for why.
    let ga_program_kp = Keypair::new();
    let ga_program_id = ga_program_kp.pubkey();
    eprintln!(
        "[smoke] global_accountant program_id={ga_program_id} (fresh)\n[smoke] \
         solana_noreplay program_id={NOREPLAY_PROGRAM_ID} (pinned canonical)"
    );

    // ----- Phase 3: payer + authority funding. Both must be airdropped before
    // any tx that consumes lamports.
    let payer = Keypair::new();
    let authority = Keypair::new();
    for (label, pk) in [("payer", payer.pubkey()), ("authority", authority.pubkey())] {
        let sig = rpc
            .request_airdrop(&pk, 10_000_000_000)
            .unwrap_or_else(|e| panic!("airdrop {label}: {e}"));
        await_confirmed(
            &format!("airdrop {label}"),
            Duration::from_secs(10),
            || rpc.confirm_transaction(&sig),
        );
    }
    let payer_starting = rpc
        .get_balance(&payer.pubkey())
        .expect("payer balance after airdrop");
    assert!(payer_starting >= 10_000_000_000, "payer funded");

    // ----- Phase 4: deploy both .so's via the surfnet_writeProgram cheatcode.
    // Order does not matter — surfpool's writeProgram path is independent per
    // address. Both are pinocchio binaries so they share the same SBF loader.
    deploy_program(&rpc_url, &ga_program_id, &ga_bytes);
    deploy_program(&rpc_url, &NOREPLAY_PROGRAM_ID, &noreplay_bytes);

    let ga_acct = rpc
        .get_account(&ga_program_id)
        .expect("global_accountant program account after deploy");
    assert!(ga_acct.executable, "global_accountant is executable");
    let nr_acct = rpc
        .get_account(&NOREPLAY_PROGRAM_ID)
        .expect("solana_noreplay program account after deploy");
    assert!(nr_acct.executable, "solana_noreplay is executable");

    // ----- Phase 5: build the namespace and derive the bitmap PDA for sequence 100.
    let chain: u16 = 2;
    let mut emitter = [0u8; 32];
    emitter[0] = 0xab;
    emitter[31] = 0xcd;
    let namespace = build_namespace(chain, &emitter);
    assert_eq!(namespace.len(), 34, "wormhole namespace is 34 bytes");

    let seq_a: u64 = 100;
    let expected_bucket = seq_a / BITS_PER_BUCKET;
    let expected_bit = (seq_a % BITS_PER_BUCKET) as usize;
    assert_eq!(expected_bucket, 0);
    assert_eq!(expected_bit, 100);

    let (bitmap_pda, bitmap_bump) =
        derive_noreplay_bitmap_pda(&authority.pubkey(), &namespace, seq_a);
    eprintln!(
        "[smoke] bitmap_pda={bitmap_pda} bump={bitmap_bump} bucket={expected_bucket} \
         bit={expected_bit}"
    );

    // ----- Phase 6: CreateBitmap. Authority does NOT need to sign here.
    let create_ix = noreplay_instruction(
        DISC_CREATE_BITMAP,
        &payer.pubkey(),
        &authority.pubkey(),
        false,
        &bitmap_pda,
        &namespace,
        seq_a,
    );
    send_and_confirm(&rpc, "CreateBitmap", &[create_ix], &[&payer]);

    // Bitmap PDA must exist, be owned by noreplay, 129 bytes, all bits clear.
    let pda_after_create = rpc
        .get_account(&bitmap_pda)
        .expect("bitmap PDA after CreateBitmap");
    assert_eq!(
        pda_after_create.owner, NOREPLAY_PROGRAM_ID,
        "bitmap PDA owned by noreplay"
    );
    assert_eq!(
        pda_after_create.data.len(),
        BITMAP_ACCOUNT_SIZE,
        "bitmap PDA is exactly 129 bytes (1B stored bump + 128B bitmap)"
    );
    assert_eq!(
        pda_after_create.data[0], bitmap_bump,
        "stored bump byte matches the off-chain-derived bump"
    );
    assert!(
        pda_after_create.data[1..].iter().all(|&b| b == 0),
        "bitmap is all-zero immediately after CreateBitmap"
    );

    // ----- Phase 7: MarkUsed for sequence 100. Authority signs.
    let mark_a_ix = noreplay_instruction(
        DISC_MARK_USED,
        &payer.pubkey(),
        &authority.pubkey(),
        true,
        &bitmap_pda,
        &namespace,
        seq_a,
    );
    send_and_confirm(&rpc, "MarkUsed[100]", &[mark_a_ix], &[&payer, &authority]);

    let pda_after_mark = rpc
        .get_account(&bitmap_pda)
        .expect("bitmap PDA after MarkUsed");
    assert_eq!(
        pda_after_mark.data[0], bitmap_bump,
        "stored bump preserved after MarkUsed"
    );
    let bitmap = &pda_after_mark.data[1..];
    assert!(
        is_bit_set(bitmap, expected_bit),
        "bit at offset {expected_bit} is set after MarkUsed[100]"
    );

    // ----- Phase 8: Replay — MarkUsed[100] again. Must fail with the noreplay
    // program's replay error (`ProgramError::AccountAlreadyInitialized`).
    let replay_ix = noreplay_instruction(
        DISC_MARK_USED,
        &payer.pubkey(),
        &authority.pubkey(),
        true,
        &bitmap_pda,
        &namespace,
        seq_a,
    );
    let replay_err =
        send_expect_failure(&rpc, "MarkUsed[100] replay", &[replay_ix], &[&payer, &authority]);
    // The program returns `ProgramError::AccountAlreadyInitialized`, which the
    // runtime surfaces in logs as "instruction requires an uninitialized
    // account" (the canonical built-in error string). Match the surface form
    // loosely so a tightening of solana-client's error rendering doesn't break
    // us; the symbolic alternatives below cover other plausible renderings.
    assert!(
        replay_err.contains("instruction requires an uninitialized account")
            || replay_err.contains("AccountAlreadyInitialized")
            || replay_err.contains("account already in use")
            || replay_err.contains("custom program error"),
        "expected replay rejection from noreplay; got: {replay_err}"
    );
    eprintln!("[smoke] replay rejected as expected: {replay_err}");

    // ----- Phase 9: MarkUsed[200] — same bucket (200 / 1024 = 0), different bit.
    // Both bits must be set in the same PDA.
    let seq_b: u64 = 200;
    let (bitmap_pda_b, _) = derive_noreplay_bitmap_pda(&authority.pubkey(), &namespace, seq_b);
    assert_eq!(
        bitmap_pda_b, bitmap_pda,
        "sequences sharing a bucket resolve to the same PDA address"
    );

    let mark_b_ix = noreplay_instruction(
        DISC_MARK_USED,
        &payer.pubkey(),
        &authority.pubkey(),
        true,
        &bitmap_pda,
        &namespace,
        seq_b,
    );
    send_and_confirm(&rpc, "MarkUsed[200]", &[mark_b_ix], &[&payer, &authority]);

    let pda_after_mark_b = rpc
        .get_account(&bitmap_pda)
        .expect("bitmap PDA after MarkUsed[200]");
    let bitmap = &pda_after_mark_b.data[1..];
    let bit_b = (seq_b % BITS_PER_BUCKET) as usize;
    assert!(
        is_bit_set(bitmap, expected_bit),
        "bit 100 still set after MarkUsed[200]"
    );
    assert!(
        is_bit_set(bitmap, bit_b),
        "bit {bit_b} set after MarkUsed[200]"
    );

    eprintln!("[smoke] all phases green. SurfpoolGuard cleanup on Drop.");
    // SurfpoolGuard's Drop SIGKILLs the subprocess; nothing further to do.
}

/// Send a happy-path tx and panic if it does not confirm. Wraps the boilerplate
/// of fetching a fresh blockhash and converting `send_and_confirm` errors into a
/// useful panic message.
fn send_and_confirm(
    rpc: &solana_client::rpc_client::RpcClient,
    label: &str,
    ixs: &[Instruction],
    signers: &[&Keypair],
) {
    let blockhash = rpc
        .get_latest_blockhash()
        .unwrap_or_else(|e| panic!("blockhash for {label}: {e}"));
    let tx = Transaction::new_signed_with_payer(
        ixs,
        Some(&signers[0].pubkey()),
        signers,
        blockhash,
    );
    // surfpool's preflight occasionally mis-flags signer bits on freshly-deployed
    // pinocchio programs; skip preflight and rely on the on-chain run for the
    // signer check. The transaction is still re-verified at land time.
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
    eprintln!("[smoke] {label} tx={sig}");
}

/// Send a tx that is expected to fail; return the surfaced error string for
/// pattern matching. Panics if the tx unexpectedly confirms.
fn send_expect_failure(
    rpc: &solana_client::rpc_client::RpcClient,
    label: &str,
    ixs: &[Instruction],
    signers: &[&Keypair],
) -> String {
    let blockhash = rpc
        .get_latest_blockhash()
        .unwrap_or_else(|e| panic!("blockhash for {label}: {e}"));
    let tx = Transaction::new_signed_with_payer(
        ixs,
        Some(&signers[0].pubkey()),
        signers,
        blockhash,
    );
    match rpc.send_and_confirm_transaction(&tx) {
        Ok(sig) => panic!("{label} unexpectedly confirmed: tx={sig}"),
        Err(e) => e.to_string(),
    }
}

/// Test a single bit in the noreplay bitmap. The bitmap layout is bit `i` lives
/// in byte `i / 8` at offset `i % 8` (LSB-first). Mirrors
/// `solana_noreplay::state::BitmapAccount::is_used`.
fn is_bit_set(bitmap: &[u8], bit: usize) -> bool {
    let byte = bit / 8;
    let offset = bit % 8;
    bitmap[byte] & (1 << offset) != 0
}

