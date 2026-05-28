//! Integration tests for `submit_vaas`.
//!
//! Gated on the paired `(mock-vaa, test-only-open-digest, mock-noreplay)`
//! feature trio so the in-process mollusk runs skip the Verify VAA Shim CPI,
//! expose the `open_digest` cross-check entrypoint, and substitute a
//! single-byte NoReplay sentinel for the real CPI. The surfpool e2e test
//! (`surfpool_e2e_submit_vaas.rs`) drives the real CPI path.
//!
//! Test surface:
//!
//! - happy-path transfer: balances credited / debited, NoReplay flipped,
//!   DigestAccount opened.
//! - duplicate replay: second `submit_vaas` for the same `(chain, emitter,
//!   sequence)` rejects via the NoReplay pre-check.
//! - attest payload (action 0x02): no balance work but commit completes.
//! - body / Shim digest mismatch: real CPI would reject; we exercise the
//!   surrounding plumbing under the mock branch and pin the digest path
//!   shape (the e2e test is the canonical real-CPI guard).
//! - lazy-init of destination Account PDA.
//! - balance underflow: whole tx reverts (NoReplay stays unset, DigestAccount
//!   stays unopened) — Solana atomicity.

#![allow(clippy::too_many_arguments)]

use {
    global_accountant_definitions::{
        BalanceAccountLayout, DigestAccountLayout, GlobalAccountantError,
        Instruction as IxDiscriminator, Uint256, ACCOUNT_SEED_PREFIX, DIGEST_SEED_PREFIX,
    },
    mollusk_svm::{program::keyed_account_for_system_program, result::ProgramResult, Mollusk},
    solana_account::Account,
    solana_instruction::{AccountMeta, Instruction},
    solana_pubkey::Pubkey,
};

const PROGRAM_NAME: &str = "global_accountant";

fn program_id() -> Pubkey {
    // Fixed program id so PDA derivations match the on-chain program's view.
    Pubkey::new_from_array([7u8; 32])
}

fn mollusk() -> Mollusk {
    Mollusk::new(&program_id(), PROGRAM_NAME)
}

fn system_program_id() -> Pubkey {
    keyed_account_for_system_program().0
}

// ============================================================================
// PDA derivation helpers
// ============================================================================

fn derive_digest_pda(chain: u16, emitter: &[u8; 32], sequence: u64) -> (Pubkey, u8) {
    let chain_be = chain.to_be_bytes();
    let sequence_be = sequence.to_be_bytes();
    Pubkey::find_program_address(
        &[DIGEST_SEED_PREFIX, &chain_be, emitter, &sequence_be],
        &program_id(),
    )
}

fn derive_account_pda(chain: u16, token_chain: u16, token_address: &[u8; 32]) -> (Pubkey, u8) {
    let chain_be = chain.to_be_bytes();
    let token_chain_be = token_chain.to_be_bytes();
    Pubkey::find_program_address(
        &[ACCOUNT_SEED_PREFIX, &chain_be, &token_chain_be, token_address],
        &program_id(),
    )
}

/// Host-side `keccak256(keccak256(body))` — the Wormhole digest convention.
fn double_keccak256_host(body: &[u8]) -> [u8; 32] {
    let inner = solana_keccak_hasher::hashv(&[body]).to_bytes();
    solana_keccak_hasher::hashv(&[&inner]).to_bytes()
}

// ============================================================================
// VAA body builders — mirror the helpers in `submit_observations.rs` tests.
// ============================================================================

fn build_attest_body(emitter_chain: u16, emitter_address: &[u8; 32], sequence: u64) -> Vec<u8> {
    let mut body = vec![0u8; 52];
    body[8..10].copy_from_slice(&emitter_chain.to_be_bytes());
    body[10..42].copy_from_slice(emitter_address);
    body[42..50].copy_from_slice(&sequence.to_be_bytes());
    body[51] = 0x02;
    body
}

fn build_transfer_body(
    emitter_chain: u16,
    emitter_address: &[u8; 32],
    sequence: u64,
    amount: u128,
    token_chain: u16,
    token_address: &[u8; 32],
    recipient_chain: u16,
) -> Vec<u8> {
    let mut body = vec![0u8; 51 + 133];
    body[8..10].copy_from_slice(&emitter_chain.to_be_bytes());
    body[10..42].copy_from_slice(emitter_address);
    body[42..50].copy_from_slice(&sequence.to_be_bytes());
    body[51] = 0x01;
    body[52 + 16..52 + 32].copy_from_slice(&amount.to_be_bytes());
    body[84..116].copy_from_slice(token_address);
    body[116..118].copy_from_slice(&token_chain.to_be_bytes());
    body[118] = 0xAB;
    body[149] = 0xCD;
    body[150..152].copy_from_slice(&recipient_chain.to_be_bytes());
    body
}

// ============================================================================
// Wire builders
// ============================================================================

fn submit_vaas_ix_data(guardian_set_bump: u8, body: &[u8]) -> Vec<u8> {
    // Wire shape: 1-byte discriminator + 1-byte guardian_set_bump + 2-byte
    // body length LE + body bytes. Mirrors
    // `submit_vaas.rs::SUBMIT_VAAS_FIXED_LEN`.
    let mut data = Vec::with_capacity(1 + 1 + 2 + body.len());
    data.push(IxDiscriminator::SubmitVaas as u8);
    data.push(guardian_set_bump);
    data.extend_from_slice(&(body.len() as u16).to_le_bytes());
    data.extend_from_slice(body);
    data
}

// ============================================================================
// Account fixtures
// ============================================================================

fn system_owned_account(lamports: u64) -> Account {
    Account {
        lamports,
        data: vec![],
        owner: system_program_id(),
        executable: false,
        rent_epoch: 0,
    }
}

fn uninitialised_pda_account() -> Account {
    system_owned_account(0)
}

fn noreplay_bucket_unmarked() -> Account {
    Account {
        lamports: 1_000_000,
        data: vec![0u8; 1],
        owner: program_id(),
        executable: false,
        rent_epoch: 0,
    }
}

fn noreplay_bucket_marked() -> Account {
    Account {
        lamports: 1_000_000,
        data: vec![0x01u8; 1],
        owner: program_id(),
        executable: false,
        rent_epoch: 0,
    }
}

/// Stand-in for a posted `GuardianSignatures` PDA. Under `mock-vaa` the Shim
/// CPI is a no-op so the account data is irrelevant — any read-only slot
/// satisfies the runtime account-meta. Using a Shim-program-owned account
/// keeps the address shape consistent with what production observes; the
/// owner choice is documentary, the mock branch never reads it.
fn fabricated_guardian_signatures_account() -> Account {
    Account {
        lamports: 1_572_960, // rent-exempt-ish, irrelevant
        data: vec![0u8; 16],
        owner: Pubkey::new_from_array(
            global_accountant_definitions::VERIFY_VAA_SHIM_PROGRAM_ID,
        ),
        executable: false,
        rent_epoch: 0,
    }
}

fn fabricated_guardian_set_account() -> Account {
    // The mock-vaa branch doesn't read the guardian-set bytes; provide a
    // minimal placeholder so the account meta is satisfied.
    Account {
        lamports: 1_000_000,
        data: vec![0u8; 8],
        owner: Pubkey::new_from_array([0xCCu8; 32]),
        executable: false,
        rent_epoch: 0,
    }
}

// ============================================================================
// Scenario builder
// ============================================================================

#[derive(Clone)]
struct Scenario {
    chain: u16,
    emitter: [u8; 32],
    sequence: u64,
    body: Vec<u8>,
    digest: [u8; 32],
    guardian_set_bump: u8,
    submitter: Pubkey,
    digest_pda: Pubkey,
    verify_vaa_shim_program: Pubkey,
    guardian_set_pubkey: Pubkey,
    guardian_signatures_pubkey: Pubkey,
    noreplay_bucket_pubkey: Pubkey,
    noreplay_program_pubkey: Pubkey,
    noreplay_authority_pubkey: Pubkey,
    source_account_pubkey: Pubkey,
    dest_account_pubkey: Pubkey,
}

impl Scenario {
    /// Default Attest-payload scenario. `with_transfer_body` swaps in a real
    /// Transfer body and re-derives Account PDAs.
    fn new(seed: u8) -> Self {
        let chain: u16 = 2;
        let mut emitter = [0u8; 32];
        emitter[31] = 0x77;
        emitter[0] = seed; // per-scenario emitter so PDA tests are isolated
        let sequence: u64 = 0x0000_0000_0000_0042;

        let body = build_attest_body(chain, &emitter, sequence);
        let digest = double_keccak256_host(&body);

        let submitter = Pubkey::new_from_array([0x11u8; 32]);
        let (digest_pda, _digest_bump) = derive_digest_pda(chain, &emitter, sequence);
        let noreplay_authority_pubkey = Pubkey::new_from_array([0xC4u8; 32]);

        Self {
            chain,
            emitter,
            sequence,
            body,
            digest,
            // Bump under `mock-vaa` is documentary; the mock branch never
            // reads it. Pin a recognisable value rather than 0/255 so a
            // future mistake reading the byte surfaces as a logged "255".
            guardian_set_bump: 254,
            submitter,
            digest_pda,
            verify_vaa_shim_program: Pubkey::new_from_array(
                global_accountant_definitions::VERIFY_VAA_SHIM_PROGRAM_ID,
            ),
            guardian_set_pubkey: Pubkey::new_from_array([0xC1u8; 32]),
            guardian_signatures_pubkey: Pubkey::new_from_array([0xC5u8; 32]),
            noreplay_bucket_pubkey: Pubkey::new_from_array([0xC2u8; 32]),
            noreplay_program_pubkey: Pubkey::new_from_array([0xC3u8; 32]),
            noreplay_authority_pubkey,
            // Sentinel: Attest payloads never touch slots 8/9.
            source_account_pubkey: noreplay_authority_pubkey,
            dest_account_pubkey: noreplay_authority_pubkey,
        }
    }

    fn with_transfer_body(
        seed: u8,
        amount: u128,
        token_chain: u16,
        token_address: [u8; 32],
        recipient_chain: u16,
    ) -> Self {
        let mut base = Self::new(seed);
        base.body = build_transfer_body(
            base.chain,
            &base.emitter,
            base.sequence,
            amount,
            token_chain,
            &token_address,
            recipient_chain,
        );
        base.digest = double_keccak256_host(&base.body);
        let (digest_pda, _) = derive_digest_pda(base.chain, &base.emitter, base.sequence);
        base.digest_pda = digest_pda;
        let (src, _) = derive_account_pda(base.chain, token_chain, &token_address);
        let (dst, _) = derive_account_pda(recipient_chain, token_chain, &token_address);
        base.source_account_pubkey = src;
        base.dest_account_pubkey = dst;
        base
    }

    fn account_metas(&self) -> Vec<AccountMeta> {
        vec![
            AccountMeta::new(self.submitter, true),
            AccountMeta::new_readonly(self.verify_vaa_shim_program, false),
            AccountMeta::new_readonly(self.guardian_set_pubkey, false),
            AccountMeta::new_readonly(self.guardian_signatures_pubkey, false),
            AccountMeta::new(self.digest_pda, false),
            AccountMeta::new(self.noreplay_bucket_pubkey, false),
            AccountMeta::new_readonly(self.noreplay_program_pubkey, false),
            AccountMeta::new_readonly(self.noreplay_authority_pubkey, false),
            AccountMeta::new(self.source_account_pubkey, false),
            AccountMeta::new(self.dest_account_pubkey, false),
            AccountMeta::new_readonly(system_program_id(), false),
        ]
    }

    fn initial_accounts(&self) -> Vec<(Pubkey, Account)> {
        let mut accounts = vec![
            (self.submitter, system_owned_account(50_000_000_000)),
            (
                self.verify_vaa_shim_program,
                Account {
                    lamports: 1,
                    data: vec![],
                    owner: Pubkey::new_from_array([0xAAu8; 32]),
                    executable: true,
                    rent_epoch: 0,
                },
            ),
            (self.guardian_set_pubkey, fabricated_guardian_set_account()),
            (
                self.guardian_signatures_pubkey,
                fabricated_guardian_signatures_account(),
            ),
            (self.digest_pda, uninitialised_pda_account()),
            (self.noreplay_bucket_pubkey, noreplay_bucket_unmarked()),
            (self.noreplay_program_pubkey, system_owned_account(0)),
            (self.noreplay_authority_pubkey, system_owned_account(0)),
        ];
        if self.source_account_pubkey != self.noreplay_authority_pubkey {
            accounts.push((self.source_account_pubkey, uninitialised_pda_account()));
        }
        if self.dest_account_pubkey != self.noreplay_authority_pubkey
            && self.dest_account_pubkey != self.source_account_pubkey
        {
            accounts.push((self.dest_account_pubkey, uninitialised_pda_account()));
        }
        accounts.push(keyed_account_for_system_program());
        accounts
    }

    fn submit(
        &self,
        mollusk: &Mollusk,
        starting_accounts: Vec<(Pubkey, Account)>,
    ) -> mollusk_svm::result::InstructionResult {
        let ix = Instruction::new_with_bytes(
            program_id(),
            &submit_vaas_ix_data(self.guardian_set_bump, &self.body),
            self.account_metas(),
        );
        mollusk.process_instruction(&ix, &starting_accounts)
    }
}

fn find_account<'a>(accounts: &'a [(Pubkey, Account)], key: &Pubkey) -> &'a Account {
    &accounts
        .iter()
        .find(|(k, _)| k == key)
        .unwrap_or_else(|| panic!("account {key} not in result list"))
        .1
}

// ============================================================================
// Tests
// ============================================================================

#[test]
fn submit_vaas_transfer_commits_balances_and_opens_digest() {
    // Happy path: Ethereum (chain=2) emits a Transfer of USDC
    // (token_chain=2, i.e. Ethereum-native) to Solana (chain=1). The
    // accountant must lock_or_burn on source (native ⇒ credit) and
    // unlock_or_mint on dest (wrapped ⇒ credit). NoReplay flips,
    // DigestAccount opens.
    let mollusk = mollusk();
    let token_address = [0x77u8; 32];
    let scenario = Scenario::with_transfer_body(
        0xA0,
        500_000u128,
        2, // token_chain = Ethereum (token-native)
        token_address,
        1, // recipient_chain = Solana (wrapped destination)
    );

    let result = scenario.submit(&mollusk, scenario.initial_accounts());
    assert!(
        matches!(result.program_result, ProgramResult::Success),
        "expected Success, got {:?}",
        result.program_result
    );

    // NoReplay flipped to marked.
    let bucket = find_account(&result.resulting_accounts, &scenario.noreplay_bucket_pubkey);
    assert_eq!(bucket.data[0], 0x01, "NoReplay flipped on submit_vaas commit");

    // DigestAccount opened with the expected digest.
    let digest = find_account(&result.resulting_accounts, &scenario.digest_pda);
    assert_eq!(
        digest.owner,
        program_id(),
        "digest PDA owned by program after submit_vaas"
    );
    assert_eq!(digest.data.len(), DigestAccountLayout::LEN);
    let stored: &DigestAccountLayout = bytemuck::from_bytes(&digest.data);
    assert_eq!(stored.digest, scenario.digest);
    assert_eq!(stored.chain, scenario.chain);
    assert_eq!(stored.emitter, scenario.emitter);
    assert_eq!(stored.sequence, scenario.sequence);
    assert_eq!(
        stored.guardian_set_index, 0,
        "submit_vaas records gsi=0 sentinel"
    );

    // Source-chain Account: chain == token_chain == 2 ⇒ native lock ⇒ credit.
    let src = find_account(&result.resulting_accounts, &scenario.source_account_pubkey);
    assert_eq!(src.owner, program_id(), "source Account PDA owned by program");
    let src_layout: &BalanceAccountLayout = bytemuck::from_bytes(&src.data);
    assert_eq!(src_layout.balance, Uint256::from_u128(500_000));

    // Dest-chain Account: chain (1) != token_chain (2) ⇒ wrapped mint ⇒ credit.
    let dst = find_account(&result.resulting_accounts, &scenario.dest_account_pubkey);
    assert_eq!(dst.owner, program_id(), "dest Account PDA owned by program");
    let dst_layout: &BalanceAccountLayout = bytemuck::from_bytes(&dst.data);
    assert_eq!(dst_layout.balance, Uint256::from_u128(500_000));
}

#[test]
fn submit_vaas_rejects_duplicate_after_noreplay_set() {
    // Two `submit_vaas` calls for the same `(chain, emitter, sequence)`:
    // first commits, second must reject with `AlreadyAccounted` via the
    // NoReplay pre-check. Mirrors CosmWasm's `handle_vaa`
    // `DuplicateMessage` short-circuit.
    let mollusk = mollusk();
    let token_address = [0x99u8; 32];
    let scenario = Scenario::with_transfer_body(0xA1, 100u128, 2, token_address, 1);

    let first = scenario.submit(&mollusk, scenario.initial_accounts());
    assert!(matches!(first.program_result, ProgramResult::Success));

    let second = scenario.submit(&mollusk, first.resulting_accounts.clone());
    match second.program_result {
        ProgramResult::Failure(err) => {
            let code = u64::from(err) as u32;
            assert_eq!(
                code,
                GlobalAccountantError::AlreadyAccounted as u32,
                "expected AlreadyAccounted, got {code:?}"
            );
        }
        other => panic!("expected Failure(AlreadyAccounted), got {other:?}"),
    }
}

#[test]
fn submit_vaas_with_attest_payload_skips_balance_work_but_marks_replay() {
    // Action 0x02 (Attest) carries no transfer data. The program must
    // commit (NoReplay flip + DigestAccount open) but touch neither
    // Account PDA. The sentinel slots (= noreplay-authority) stay
    // system-owned.
    let mollusk = mollusk();
    let scenario = Scenario::new(0xA2);

    let result = scenario.submit(&mollusk, scenario.initial_accounts());
    assert!(
        matches!(result.program_result, ProgramResult::Success),
        "attest submit must succeed, got {:?}",
        result.program_result
    );

    // NoReplay flipped, DigestAccount opened.
    let bucket = find_account(&result.resulting_accounts, &scenario.noreplay_bucket_pubkey);
    assert_eq!(bucket.data[0], 0x01);
    let digest = find_account(&result.resulting_accounts, &scenario.digest_pda);
    assert_eq!(digest.owner, program_id());
    let stored: &DigestAccountLayout = bytemuck::from_bytes(&digest.data);
    assert_eq!(stored.digest, scenario.digest);

    // Sentinel slots stay system-owned (program never touched them).
    assert_eq!(scenario.source_account_pubkey, scenario.noreplay_authority_pubkey);
    let sentinel = find_account(
        &result.resulting_accounts,
        &scenario.noreplay_authority_pubkey,
    );
    assert_eq!(
        sentinel.owner,
        system_program_id(),
        "sentinel slot stays system-owned across attest commit"
    );
    assert!(sentinel.data.is_empty());
}

#[test]
fn submit_vaas_with_pre_marked_noreplay_rejects_before_state_mutation() {
    // The NoReplay pre-check fires before the Shim CPI's mock-branch return.
    // We pre-flip the bucket and submit: the program must reject with
    // `AlreadyAccounted` and leave the DigestAccount untouched.
    //
    // This is the documented behaviour parallel to CosmWasm's
    // `DuplicateMessage` short-circuit when the same `(chain, emitter,
    // sequence)` has already been committed via any path.
    let mollusk = mollusk();
    let scenario = Scenario::new(0xA3);

    let mut accounts = scenario.initial_accounts();
    for (key, acct) in accounts.iter_mut() {
        if *key == scenario.noreplay_bucket_pubkey {
            *acct = noreplay_bucket_marked();
        }
    }

    let result = scenario.submit(&mollusk, accounts);
    match result.program_result {
        ProgramResult::Failure(err) => {
            let code = u64::from(err) as u32;
            assert_eq!(
                code,
                GlobalAccountantError::AlreadyAccounted as u32,
                "expected AlreadyAccounted, got {code:?}"
            );
        }
        other => panic!("expected Failure(AlreadyAccounted), got {other:?}"),
    }
    // DigestAccount must not have opened.
    let digest = find_account(&result.resulting_accounts, &scenario.digest_pda);
    assert!(
        digest.data.is_empty(),
        "DigestAccount must not open when NoReplay short-circuits"
    );
}

#[test]
fn submit_vaas_lazy_inits_destination_account() {
    // Fresh destination Account PDA (system-owned, empty data) — must
    // lazy-init under the program at the same canonical address.
    let mollusk = mollusk();
    let token_address = [0x42u8; 32];
    let scenario = Scenario::with_transfer_body(0xA4, 9_999u128, 2, token_address, 1);

    let initial = scenario.initial_accounts();
    let dst_pre = find_account(&initial, &scenario.dest_account_pubkey);
    assert_eq!(dst_pre.owner, system_program_id());
    assert!(dst_pre.data.is_empty());

    let result = scenario.submit(&mollusk, initial);
    assert!(matches!(result.program_result, ProgramResult::Success));

    let dst_post = find_account(&result.resulting_accounts, &scenario.dest_account_pubkey);
    assert_eq!(
        dst_post.owner,
        program_id(),
        "dest lazy-init flips owner to program"
    );
    assert_eq!(dst_post.data.len(), BalanceAccountLayout::LEN);
    let layout: &BalanceAccountLayout = bytemuck::from_bytes(&dst_post.data);
    assert_eq!(layout.balance, Uint256::from_u128(9_999));
}

#[test]
fn submit_vaas_with_balance_underflow_reverts() {
    // Wrapped-chain debit larger than the on-chain balance must surface
    // `BalanceUnderflow`. Whole tx reverts: NoReplay stays unset,
    // DigestAccount stays unopened.
    let mollusk = mollusk();
    let token_address = [0x88u8; 32];
    // Solana (chain=1) emits a transfer of Ethereum-native USDC
    // (token_chain=2) back to Ethereum. Source = (chain=1, token_chain=2)
    // ⇒ wrapped ⇒ lock_or_burn DEBITs. With a fresh source balance of
    // zero, the debit underflows.
    let mut scenario = Scenario::with_transfer_body(0xA5, 1_000u128, 2, token_address, 2);
    scenario.chain = 1;
    scenario.body = build_transfer_body(
        scenario.chain,
        &scenario.emitter,
        scenario.sequence,
        1_000,
        2,
        &token_address,
        2,
    );
    scenario.digest = double_keccak256_host(&scenario.body);
    let (digest_pda, _) = derive_digest_pda(scenario.chain, &scenario.emitter, scenario.sequence);
    scenario.digest_pda = digest_pda;
    let (src, _) = derive_account_pda(1, 2, &token_address);
    let (dst, _) = derive_account_pda(2, 2, &token_address);
    scenario.source_account_pubkey = src;
    scenario.dest_account_pubkey = dst;

    let result = scenario.submit(&mollusk, scenario.initial_accounts());
    match result.program_result {
        ProgramResult::Failure(err) => {
            let code = u64::from(err) as u32;
            assert_eq!(
                code,
                GlobalAccountantError::BalanceUnderflow as u32,
                "expected BalanceUnderflow, got {code:?}"
            );
        }
        other => panic!("expected Failure(BalanceUnderflow), got {other:?}"),
    }
    // Transactional integrity: NoReplay must not flip, DigestAccount must
    // not open. Solana atomicity guarantees this; the assertion is
    // belt-and-braces.
    let bucket = find_account(&result.resulting_accounts, &scenario.noreplay_bucket_pubkey);
    assert_eq!(bucket.data[0], 0u8);
    let digest = find_account(&result.resulting_accounts, &scenario.digest_pda);
    assert!(digest.data.is_empty());
}

#[test]
fn submit_vaas_with_short_body_rejects() {
    // Body shorter than the 51-byte header + 1 action byte must reject as
    // `InvalidInstructionData`. Guards against malformed callers.
    let mollusk = mollusk();
    let scenario = Scenario::new(0xA6);

    // Build a too-short body (50 bytes) and re-encode the wire data.
    let short_body = vec![0u8; 50];
    let mut wire = Vec::with_capacity(1 + 1 + 2 + short_body.len());
    wire.push(IxDiscriminator::SubmitVaas as u8);
    wire.push(scenario.guardian_set_bump);
    wire.extend_from_slice(&(short_body.len() as u16).to_le_bytes());
    wire.extend_from_slice(&short_body);
    let ix = Instruction::new_with_bytes(program_id(), &wire, scenario.account_metas());
    let result = mollusk.process_instruction(&ix, &scenario.initial_accounts());
    match result.program_result {
        ProgramResult::Failure(err) => {
            let code = u64::from(err) as u32;
            assert_eq!(
                code,
                GlobalAccountantError::InvalidInstructionData as u32,
                "expected InvalidInstructionData, got {code:?}"
            );
        }
        other => panic!("expected Failure(InvalidInstructionData), got {other:?}"),
    }
}
