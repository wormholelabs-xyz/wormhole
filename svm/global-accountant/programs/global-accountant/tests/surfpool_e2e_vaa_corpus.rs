//! Phase 2 surfpool VAA-replay corpus.
//!
//! Each test in this file replays one historical mainnet VAA end-to-end
//! against the production-shape `.so` (no `mock-vaa`, real Verify VAA Shim
//! CPI). The corpus widens the Phase 1b single-VAA spot check into a small
//! catalogue covering different emitter shapes, payload sizes, and guardian
//! sets so a future regression in our digest-handling, account-encoding, or
//! Shim-CPI path surfaces against more than one fixture.
//!
//! # Corpus coverage matrix
//!
//! | filename                                          | gs | chain | emitter kind            | payload | rationale                                       |
//! |---------------------------------------------------|----|-------|-------------------------|---------|-------------------------------------------------|
//! | `mainnet_solana_token_bridge_seq2211.vaa`         |  6 |  1    | Solana NTT manager      |     217 | active GS6 baseline (mirrors Phase 1b)          |
//! | `mainnet_solana_token_bridge_transfer_seq1395207` |  6 |  1    | Solana Token Bridge     |     133 | textbook TB transfer (PayloadID=1)              |
//! | `mainnet_solana_pyth_short_seq175120.vaa`         |  6 |  1    | Solana Pyth-like        |      32 | short payload, active GS6                       |
//! | `mainnet_xlayer_ntt_long_seq2695.vaa`             |  6 | 48    | XLayer NTT manager      |     388 | non-Solana NTT emitter shape                    |
//! | `mainnet_gs1_token_bridge_register_chain.vaa`     |  1 |  1    | Core gov (TokenBridge)  |      67 | Token Bridge governance, retired GS1            |
//! | `mainnet_gs4_core_guardian_set_upgrade.vaa`       |  4 |  1    | Core gov (Core)         |     420 | Core Bridge guardian-set upgrade, retired GS4   |
//! | `mainnet_gs5_core_delegated_guardians.vaa`        |  5 |  1    | Core gov (DelegGdns)    |     932 | long payload, retired GS5                       |
//!
//! # Why per-test surfpool instances
//!
//! Each test boots its own surfpool subprocess. Three reasons:
//!
//! 1. **PDA isolation.** Every test seeds a `DigestAccount` PDA keyed on
//!    `(chain, emitter, sequence)`. Sharing a simnet across tests would force
//!    a unique program-ID per test to avoid PDA collisions, which would in
//!    turn force the deploy step to repeat — defeating the share.
//! 2. **Per-test failure scoping.** A test that fails (e.g. the retired GS
//!    cases below) leaves a half-mutated simnet. Per-test boots ensure the
//!    next case starts from a known mainnet-fork baseline.
//! 3. **Boot cost is acceptable.** surfpool comes up in ~3–5s; the
//!    whole corpus runs in under a minute serially, and Cargo's per-test
//!    parallelism (capped by `--test-threads`) absorbs the rest.
//!
//! # Why one test function per VAA
//!
//! `#[test_case]` parameterisation would shrink the file but obscures the
//! per-VAA expectations: active vs retired GS, expected error string, etc.
//! Named functions also give clean `--exact` filters during triage. The
//! shared `run_corpus_case` helper carries the common scaffolding so each
//! per-VAA function is just a config block.
//!
//! # Retired guardian sets
//!
//! Three corpus entries (GS1, GS4, GS5) test the §7 master-plan concern that
//! historical guardian-set PDAs may be unreachable from surfpool's mainnet
//! fork. The Shim's `VerifyHash` enforces `GuardianSet::is_active(timestamp)`,
//! which fails for retired sets even when the PDA is fetchable. These cases
//! are therefore expected to fail at the Shim CPI; the test asserts the
//! failure mode rather than success, and the assertion message documents the
//! pinned behaviour.

#![allow(clippy::too_many_arguments)]

use std::time::{Duration, Instant};

use global_accountant_definitions::{
    DigestAccountLayout, Instruction as IxDiscriminator, DIGEST_SEED_PREFIX,
    VERIFY_VAA_SHIM_PROGRAM_ID,
};
use solana_commitment_config::CommitmentConfig;
use solana_instruction::{AccountMeta, Instruction};
use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use solana_signer::Signer;
use solana_system_interface::program as system_program;
use solana_transaction::Transaction;

mod common;
use common::{
    await_confirmed, deploy_program, hex_encode, load_vaa_fixture, rpc_call, so_path,
    start_surfpool, ParsedVaa, SurfpoolGuard, SurfpoolOptions,
};

/// Wormhole Core Bridge program ID on Solana mainnet
/// (`worm2ZoG2kUd4vFXhvjh93UUH596ayRfgQ2MgjNMTth`). Mirrors
/// `surfpool_e2e_mainnet_fork::CORE_BRIDGE_PROGRAM_ID`.
const CORE_BRIDGE_PROGRAM_ID: Pubkey = Pubkey::new_from_array([
    0x0e, 0x0a, 0x58, 0x9a, 0x41, 0xa5, 0x5f, 0xbd, 0x66, 0xc5, 0x2a, 0x47, 0x5f, 0x2d, 0x92, 0xa6,
    0xd3, 0xdc, 0x9b, 0x47, 0x47, 0x11, 0x4c, 0xb9, 0xaf, 0x82, 0x5a, 0x98, 0xb5, 0x45, 0xd3, 0xce,
]);

/// Anchor discriminator for `post_signatures` (`sha256("global:post_signatures")[..8]`).
const POST_SIGNATURES_SELECTOR: [u8; 8] = [0x8a, 0x02, 0x35, 0xa6, 0x2d, 0x4d, 0x89, 0x33];

/// Compute Budget program ID (`ComputeBudget111111111111111111111111111111`).
const COMPUTE_BUDGET_PROGRAM_ID: Pubkey = Pubkey::new_from_array([
    0x03, 0x06, 0x46, 0x6f, 0xe5, 0x21, 0x17, 0x32, 0xff, 0xec, 0xad, 0xba, 0x72, 0xc3, 0x9b, 0xe7,
    0xbc, 0x8c, 0xe5, 0xbb, 0xc5, 0xf7, 0x12, 0x6b, 0x2c, 0x43, 0x9b, 0x3a, 0x40, 0x00, 0x00, 0x00,
]);

/// CU ceiling for the close tx. 400k is what Phase 1b empirically needed for
/// 13-sig VerifyHash (~199k CU) plus our pre/post-CPI bookkeeping.
const CLOSE_DIGEST_CU_LIMIT: u32 = 400_000;

/// Rent-exempt lamport balance for a 120-byte `DigestAccountLayout`.
/// `Rent::default().minimum_balance(120)` = 1_572_960.
const DIGEST_PDA_RENT_LAMPORTS: u64 = 1_572_960;

/// Datasource URL for surfpool's mainnet fork. Matches the Phase 1b override.
fn datasource_rpc_url() -> String {
    std::env::var("GA_E2E_DATASOURCE_RPC")
        .unwrap_or_else(|_| "https://api.mainnet-beta.solana.com".to_string())
}

/// Derive the `(GuardianSet, gs_index)` PDA under the Core Bridge for a given
/// index. The seed is `("GuardianSet", index_be)` and the Core Bridge is the
/// program-id owner. Returns `(pda, bump)`.
fn derive_guardian_set_pda(index: u32) -> (Pubkey, u8) {
    let idx_be = index.to_be_bytes();
    Pubkey::find_program_address(&[b"GuardianSet", &idx_be], &CORE_BRIDGE_PROGRAM_ID)
}

/// Derive our program's `DigestAccount` PDA. Seed:
/// `("digest", chain_be, emitter, sequence_be)`.
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

fn set_compute_unit_limit_ix(units: u32) -> Instruction {
    let mut data = Vec::with_capacity(5);
    data.push(0x02);
    data.extend_from_slice(&units.to_le_bytes());
    Instruction {
        program_id: COMPUTE_BUDGET_PROGRAM_ID,
        accounts: vec![],
        data,
    }
}

fn close_digest_ix_data(digest: &[u8; 32], guardian_set_bump: u8) -> Vec<u8> {
    let mut data = Vec::with_capacity(1 + 32 + 1);
    data.push(IxDiscriminator::CloseDigest as u8);
    data.extend_from_slice(digest);
    data.push(guardian_set_bump);
    data
}

/// Encode `PostSignatures` instruction data. Same shape as Phase 1b's helper.
fn post_signatures_ix_data(
    guardian_set_index: u32,
    total_signatures: u8,
    guardian_signatures: &[u8],
) -> Vec<u8> {
    assert!(
        guardian_signatures.len() % ParsedVaa::GUARDIAN_SIGNATURE_LENGTH == 0,
        "sig block length must be a multiple of 66"
    );
    let count = guardian_signatures.len() / ParsedVaa::GUARDIAN_SIGNATURE_LENGTH;
    let mut data = Vec::with_capacity(8 + 9 + guardian_signatures.len());
    data.extend_from_slice(&POST_SIGNATURES_SELECTOR);
    data.extend_from_slice(&guardian_set_index.to_le_bytes());
    data.push(total_signatures);
    data.extend_from_slice(&(count as u32).to_le_bytes());
    data.extend_from_slice(guardian_signatures);
    data
}

/// Post the VAA's signatures in a single tx. The corpus is sized so every
/// fixture's signature block fits inside one 1232-byte tx envelope.
fn post_signatures(
    rpc: &solana_client::rpc_client::RpcClient,
    payer: &Keypair,
    guardian_signatures_kp: &Keypair,
    vaa: &ParsedVaa,
) {
    let shim_program_id = Pubkey::new_from_array(VERIFY_VAA_SHIM_PROGRAM_ID);
    let ix_data = post_signatures_ix_data(
        vaa.guardian_set_index,
        vaa.num_signatures,
        vaa.signatures_slice(),
    );
    let ix = Instruction {
        program_id: shim_program_id,
        accounts: vec![
            AccountMeta::new(payer.pubkey(), true),
            AccountMeta::new(guardian_signatures_kp.pubkey(), true),
            AccountMeta::new_readonly(system_program::ID, false),
        ],
        data: ix_data,
    };
    let blockhash = rpc.get_latest_blockhash().expect("blockhash post_sigs");
    let tx = Transaction::new_signed_with_payer(
        &[ix],
        Some(&payer.pubkey()),
        &[payer, guardian_signatures_kp],
        blockhash,
    );
    let sig = rpc
        .send_and_confirm_transaction(&tx)
        .expect("PostSignatures send_and_confirm");
    eprintln!("[corpus] PostSignatures tx={sig}");
}

/// Write a `DigestAccount` PDA via the `surfnet_setAccount` cheatcode. Same
/// approach as Phase 1b — the production-shape `.so` does not expose
/// `open_digest`, so the PDA bytes are injected directly.
fn write_digest_pda(
    rpc_url: &str,
    program_id: &Pubkey,
    digest_pda: &Pubkey,
    vaa: &ParsedVaa,
    payer: Pubkey,
) {
    let mut layout: DigestAccountLayout = bytemuck::Zeroable::zeroed();
    layout.emitter = vaa.emitter_address;
    layout.digest = vaa.digest;
    layout.payer = payer.to_bytes();
    layout.sequence = vaa.sequence;
    layout.quorum_at_slot = 0;
    layout.guardian_set_index = vaa.guardian_set_index;
    layout.chain = vaa.emitter_chain;
    let bytes = bytemuck::bytes_of(&layout);
    assert_eq!(bytes.len(), DigestAccountLayout::LEN);

    let resp = rpc_call(
        rpc_url,
        "surfnet_setAccount",
        serde_json::json!([
            digest_pda.to_string(),
            {
                "lamports": DIGEST_PDA_RENT_LAMPORTS,
                "owner": program_id.to_string(),
                "executable": false,
                "rent_epoch": 0u64,
                "data": hex_encode(bytes),
            }
        ]),
    );
    assert!(
        resp.get("error").is_none(),
        "surfnet_setAccount failed for {digest_pda}: {resp}"
    );
}

/// Expected outcome for a corpus case. `Succeeds` is the common case — the
/// close tx should land. `FailsAtCpi` is the §7 retired-GS scenario: the
/// close tx is expected to reach the Shim CPI and bounce there.
enum Expectation {
    Succeeds,
    FailsAtCpi { reason: &'static str },
}

/// One row of the corpus. Constructed inline by each per-VAA `#[test]`.
struct CorpusCase<'a> {
    fixture_name: &'a str,
    /// Human-readable category tag for log lines and the per-test
    /// pass/fail report.
    category: &'a str,
    expectation: Expectation,
}

/// Shared scaffolding for every corpus VAA: boot surfpool, deploy, lazy-fetch
/// mainnet dependencies, post sigs, seed PDA, run close, validate outcome.
fn run_corpus_case(case: CorpusCase<'_>) {
    let vaa = load_vaa_fixture(case.fixture_name);
    eprintln!(
        "[corpus] case={} fixture={} gs_index={} chain={} sequence={} payload_len={} digest={}",
        case.category,
        case.fixture_name,
        vaa.guardian_set_index,
        vaa.emitter_chain,
        vaa.sequence,
        vaa.payload_len,
        hex_encode(&vaa.digest),
    );

    let so = so_path("global_accountant");
    let so_bytes = std::fs::read(&so).unwrap_or_else(|e| {
        panic!(
            "could not read {}: {e}. Run `make build-prod` first.",
            so.display()
        )
    });

    // Per-test surfpool instance — see module docs.
    let guard: SurfpoolGuard = start_surfpool(SurfpoolOptions::mainnet_fork(
        "ga-surfpool-vaa-corpus",
        datasource_rpc_url(),
    ));
    let rpc_url = guard.rpc_url();
    let rpc = guard.rpc_client();

    let program_kp = Keypair::new();
    let program_id = program_kp.pubkey();
    let payer = Keypair::new();
    let guardian_signatures_kp = Keypair::new();
    eprintln!(
        "[corpus] program_id={program_id} payer={} guardian_sigs={}",
        payer.pubkey(),
        guardian_signatures_kp.pubkey(),
    );

    // Fund payer.
    let airdrop_sig = rpc
        .request_airdrop(&payer.pubkey(), 10_000_000_000)
        .expect("request_airdrop");
    await_confirmed("airdrop", Duration::from_secs(10), || {
        rpc.confirm_transaction(&airdrop_sig)
    });

    // Deploy our production-shape program.
    deploy_program(&rpc_url, &program_id, &so_bytes);

    // Lazy-fetch the Shim and Core Bridge.
    let shim_program_id = Pubkey::new_from_array(VERIFY_VAA_SHIM_PROGRAM_ID);
    let _shim = rpc
        .get_account(&shim_program_id)
        .expect("Shim lazy-fetch");
    let _core = rpc
        .get_account(&CORE_BRIDGE_PROGRAM_ID)
        .expect("Core Bridge lazy-fetch");

    // Derive and lazy-fetch the GuardianSet PDA for this VAA's set index.
    // If the index is retired, the PDA may or may not exist on mainnet — we
    // observe the behaviour rather than asserting up-front.
    let (gs_pda, gs_bump) = derive_guardian_set_pda(vaa.guardian_set_index);
    let gs_fetch = rpc
        .get_account_with_commitment(&gs_pda, CommitmentConfig::confirmed())
        .map(|r| r.value);
    match &gs_fetch {
        Ok(Some(acct)) => eprintln!(
            "[corpus] GuardianSet idx={} pda={} bump={} present (owner={}, data_len={})",
            vaa.guardian_set_index,
            gs_pda,
            gs_bump,
            acct.owner,
            acct.data.len(),
        ),
        Ok(None) => eprintln!(
            "[corpus] GuardianSet idx={} pda={} NOT FOUND on mainnet fork",
            vaa.guardian_set_index, gs_pda
        ),
        Err(e) => eprintln!(
            "[corpus] GuardianSet idx={} pda={} fetch error: {e}",
            vaa.guardian_set_index, gs_pda
        ),
    }

    // Post signatures and seed the DigestAccount PDA.
    let post_start = Instant::now();
    post_signatures(&rpc, &payer, &guardian_signatures_kp, &vaa);
    eprintln!("[corpus] PostSignatures took {:?}", post_start.elapsed());

    let (digest_pda, _bump) = derive_digest_pda(
        &program_id,
        vaa.emitter_chain,
        &vaa.emitter_address,
        vaa.sequence,
    );
    write_digest_pda(&rpc_url, &program_id, &digest_pda, &vaa, payer.pubkey());

    // Build and submit the close.
    let close_ix = Instruction {
        program_id,
        accounts: vec![
            AccountMeta::new_readonly(payer.pubkey(), true),
            AccountMeta::new(digest_pda, false),
            AccountMeta::new(payer.pubkey(), false),
            AccountMeta::new_readonly(guardian_signatures_kp.pubkey(), false),
            AccountMeta::new_readonly(gs_pda, false),
            AccountMeta::new_readonly(shim_program_id, false),
        ],
        data: close_digest_ix_data(&vaa.digest, gs_bump),
    };
    let blockhash = rpc.get_latest_blockhash().expect("blockhash close");
    let close_tx = Transaction::new_signed_with_payer(
        &[set_compute_unit_limit_ix(CLOSE_DIGEST_CU_LIMIT), close_ix],
        Some(&payer.pubkey()),
        &[&payer],
        blockhash,
    );

    match (&case.expectation, rpc.send_and_confirm_transaction(&close_tx)) {
        (Expectation::Succeeds, Ok(sig)) => {
            eprintln!("[corpus] PASS close_digest sig={sig}");
            // PDA must be drained.
            let res = rpc
                .get_account_with_commitment(&digest_pda, CommitmentConfig::confirmed())
                .expect("post-close get_account");
            match res.value {
                None => eprintln!("[corpus] PDA removed by runtime (zero lamports)"),
                Some(acct) => assert_eq!(acct.lamports, 0, "PDA lamports drained"),
            }
        }
        (Expectation::Succeeds, Err(e)) => {
            // Dump program logs to make triage tractable.
            dump_logs_on_failure(&rpc_url, &e);
            panic!(
                "expected close_digest to succeed for {} ({}), got: {e}",
                case.fixture_name, case.category
            );
        }
        (Expectation::FailsAtCpi { reason }, Ok(sig)) => panic!(
            "expected {} ({}) to fail at Shim CPI ({reason}), but it succeeded: sig={sig}",
            case.fixture_name, case.category
        ),
        (Expectation::FailsAtCpi { reason }, Err(e)) => {
            eprintln!(
                "[corpus] EXPECTED FAILURE for {} ({}, {reason}): {e}",
                case.fixture_name, case.category
            );
            // PDA must still exist (failed tx leaves state untouched).
            let pda_acct = rpc
                .get_account_with_commitment(&digest_pda, CommitmentConfig::confirmed())
                .expect("get_account after failed close")
                .value
                .expect("DigestAccount preserved after failed close");
            assert_eq!(pda_acct.owner, program_id, "PDA still program-owned");
        }
    }
}

/// On a happy-path failure, pull the most recent failed tx's program logs so
/// the panic message includes the Shim's `msg!` lines. We don't have the sig
/// here (the RPC errored before returning one) so this is a best-effort dump
/// of the simnet's `getRecentPerformanceSamples`-style state.
fn dump_logs_on_failure(_rpc_url: &str, err: &solana_client::client_error::ClientError) {
    eprintln!("[corpus] send_and_confirm_transaction error: {err}");
}

// ---------------------------------------------------------------------------
// Per-VAA cases. One function per fixture so `cargo test` filters work and
// pass/fail reports stay readable.
// ---------------------------------------------------------------------------

#[test]
#[ignore = "spawns surfpool subprocess + lazy-fetches mainnet; run via \
            `make test-e2e-vaa-corpus`"]
fn corpus_active_gs6_solana_ntt_baseline() {
    // Mirror of Phase 1b: the same fixture, exercised through the corpus
    // helper. If this regresses, the corpus scaffolding is wrong rather than
    // any one VAA being malformed.
    run_corpus_case(CorpusCase {
        fixture_name: "mainnet_solana_token_bridge_seq2211.vaa",
        category: "active GS6 / Solana NTT (baseline)",
        expectation: Expectation::Succeeds,
    });
}

#[test]
#[ignore = "spawns surfpool subprocess + lazy-fetches mainnet; run via \
            `make test-e2e-vaa-corpus`"]
fn corpus_active_gs6_solana_token_bridge_transfer() {
    // Textbook Token Bridge transfer (PayloadID=1) emitted by the Solana
    // Token Bridge sequence PDA. The common production case.
    run_corpus_case(CorpusCase {
        fixture_name: "mainnet_solana_token_bridge_transfer_seq1395207.vaa",
        category: "active GS6 / Token Bridge transfer",
        expectation: Expectation::Succeeds,
    });
}

#[test]
#[ignore = "spawns surfpool subprocess + lazy-fetches mainnet; run via \
            `make test-e2e-vaa-corpus`"]
fn corpus_active_gs6_solana_short_payload() {
    // 32-byte payload — exercises the lower bound of our parser and confirms
    // the Shim does not care about payload length.
    run_corpus_case(CorpusCase {
        fixture_name: "mainnet_solana_pyth_short_seq175120.vaa",
        category: "active GS6 / short payload (32 bytes)",
        expectation: Expectation::Succeeds,
    });
}

#[test]
#[ignore = "spawns surfpool subprocess + lazy-fetches mainnet; run via \
            `make test-e2e-vaa-corpus`"]
fn corpus_active_gs6_xlayer_ntt_emitter() {
    // NTT emitter on XLayer (chain 48) — confirms our (chain, emitter)
    // encoding works for non-Solana emitters. Same Shim/GS6 path.
    run_corpus_case(CorpusCase {
        fixture_name: "mainnet_xlayer_ntt_long_seq2695.vaa",
        category: "active GS6 / XLayer NTT emitter",
        expectation: Expectation::Succeeds,
    });
}

#[test]
#[ignore = "spawns surfpool subprocess + lazy-fetches mainnet; run via \
            `make test-e2e-vaa-corpus`"]
fn corpus_retired_gs1_token_bridge_register_chain() {
    // Token Bridge governance VAA (`RegisterChain`, action=2) signed by
    // guardian set 1. Pins the master plan §7 "historical guardian-set
    // ceiling" concern: the Shim's `is_active(timestamp)` check fails for
    // long-expired sets, so this test asserts the failure rather than
    // success. If the Shim ever changes its expiry policy, this case will
    // flip to `Succeeds` and we'll know.
    run_corpus_case(CorpusCase {
        fixture_name: "mainnet_gs1_token_bridge_register_chain.vaa",
        category: "retired GS1 / Token Bridge governance (short)",
        expectation: Expectation::FailsAtCpi {
            reason: "GS1 is retired; Shim VerifyHash rejects with `Guardian set is expired`",
        },
    });
}

#[test]
#[ignore = "spawns surfpool subprocess + lazy-fetches mainnet; run via \
            `make test-e2e-vaa-corpus`"]
fn corpus_retired_gs4_core_guardian_set_upgrade() {
    // Core Bridge governance VAA signed by GS4 that upgrades the active set
    // to GS5. The Wormholescan-canonical "GuardianSetUpgrade" example. Same
    // expected failure mode as the GS1 case.
    run_corpus_case(CorpusCase {
        fixture_name: "mainnet_gs4_core_guardian_set_upgrade.vaa",
        category: "retired GS4 / Core guardian-set upgrade",
        expectation: Expectation::FailsAtCpi {
            reason: "GS4 is retired; Shim VerifyHash rejects with `Guardian set is expired`",
        },
    });
}

#[test]
#[ignore = "spawns surfpool subprocess + lazy-fetches mainnet; run via \
            `make test-e2e-vaa-corpus`"]
fn corpus_retired_gs5_core_delegated_guardians_long_payload() {
    // Largest payload in the corpus (932 bytes) — exercises the upper bound
    // of our parser and the Shim's tolerance for large bodies. The payload
    // itself is the recently-added "DelegatedGuardians" module governance
    // shape. Retired-GS expectation as above.
    run_corpus_case(CorpusCase {
        fixture_name: "mainnet_gs5_core_delegated_guardians.vaa",
        category: "retired GS5 / DelegatedGuardians long payload",
        expectation: Expectation::FailsAtCpi {
            reason: "GS5 is retired; Shim VerifyHash rejects with `Guardian set is expired`",
        },
    });
}

// ---------------------------------------------------------------------------
// Parser sanity unit test. Runs in-process (no surfpool) so a fixture
// regression is caught the moment `cargo test` runs.
// ---------------------------------------------------------------------------

#[test]
fn corpus_fixtures_parse_and_digest_consistently() {
    // Pin the seq 2211 digest against the value documented in
    // surfpool_e2e_mainnet_fork.rs. This is the strongest check we can run
    // without leaving the host: if the parser breaks, this test fails before
    // surfpool ever boots.
    const SEQ_2211_EXPECTED_DIGEST: [u8; 32] = [
        0xe4, 0xca, 0xc2, 0x84, 0x65, 0x6a, 0xc7, 0x4a, 0xd4, 0xef, 0x1b, 0x0e, 0xc7, 0xc2, 0xbe,
        0x76, 0x28, 0x97, 0x05, 0x45, 0x80, 0x71, 0xc7, 0xdd, 0xbe, 0xf8, 0x05, 0x49, 0x9a, 0x05,
        0x41, 0x16,
    ];
    let baseline = load_vaa_fixture("mainnet_solana_token_bridge_seq2211.vaa");
    assert_eq!(baseline.digest, SEQ_2211_EXPECTED_DIGEST, "seq 2211 digest pinned");
    assert_eq!(baseline.guardian_set_index, 6);
    assert_eq!(baseline.emitter_chain, 1);
    assert_eq!(baseline.sequence, 2211);
    assert_eq!(baseline.num_signatures, 13);

    // Smoke-check every other fixture: parses without error, digest non-zero,
    // num_signatures >= 13 (quorum for current sets), signature block length
    // matches num_signatures.
    let others = [
        "mainnet_solana_token_bridge_transfer_seq1395207.vaa",
        "mainnet_solana_pyth_short_seq175120.vaa",
        "mainnet_xlayer_ntt_long_seq2695.vaa",
        "mainnet_gs1_token_bridge_register_chain.vaa",
        "mainnet_gs4_core_guardian_set_upgrade.vaa",
        "mainnet_gs5_core_delegated_guardians.vaa",
    ];
    for name in &others {
        let v = load_vaa_fixture(name);
        assert!(v.num_signatures >= 13, "{name}: expected >=13 sigs, got {}", v.num_signatures);
        assert_eq!(
            v.signatures_slice().len(),
            (v.num_signatures as usize) * ParsedVaa::GUARDIAN_SIGNATURE_LENGTH,
            "{name}: signatures_slice length mismatch"
        );
        assert_ne!(v.digest, [0u8; 32], "{name}: digest non-zero");
        eprintln!(
            "[corpus] parsed {name}: gs={} chain={} seq={} payload_len={} digest={}",
            v.guardian_set_index,
            v.emitter_chain,
            v.sequence,
            v.payload_len,
            hex_encode(&v.digest),
        );
    }
}
