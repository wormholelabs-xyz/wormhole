use accountant_operational_core::accounts::balance;
use accountant_operational_core::cpi::noreplay::derive_bucket_pda;
use global_accountant_definitions::{
    AccountantPayerLog, BalanceAccountLayout, GlobalAccountantError, PendingObservationsLayout,
    TxId, Uint256, ACCOUNTANT_PAYER_LOG_TAG,
};
use mollusk_svm::program::create_program_account_loader_v3;
use mollusk_svm::Mollusk;
use solana_account::Account;
use solana_instruction::{AccountMeta, Instruction};
use solana_pubkey::Pubkey;
use solana_svm_log_collector::LogCollector;

use crate::common::*;

const MAX_QUORUM_BRANCH_CU: u64 = 75_000;

fn balance_layout(account: &Account) -> BalanceAccountLayout {
    layout(account)
}

/// Mollusk clock value for expiry rows.
const NOW: i64 = 1_800_000_000;

fn guardian_set_with(index: u32, guardians: &[Guardian], owner: &Pubkey) -> Account {
    guardian_set_account(index, &guardian_keys(guardians), 0, 0, owner)
}

fn submit(
    mollusk: &Mollusk,
    accounts: Vec<(Pubkey, Account)>,
    ix_data: Vec<u8>,
    metas: Vec<AccountMeta>,
) -> mollusk_svm::result::InstructionResult {
    let ix = Instruction::new_with_bytes(program_id(), &ix_data, metas);
    mollusk.process_instruction(&ix, &accounts)
}

/// `Program data: <base64>` payloads that carry the payer-log tag.
fn accpayr_logs(messages: &[String]) -> Vec<AccountantPayerLog> {
    use base64::Engine;

    messages
        .iter()
        .filter_map(|line| line.strip_prefix("Program data: "))
        .filter_map(|b64| {
            base64::engine::general_purpose::STANDARD
                .decode(b64.trim())
                .ok()
        })
        .filter(|bytes| bytes.len() >= 8 && bytes[..8] == ACCOUNTANT_PAYER_LOG_TAG)
        .map(|bytes| *AccountantPayerLog::from_bytes(&bytes).expect("ACCPAYR framing"))
        .collect()
}

#[test]
fn quorum_commit_marks_noreplay_moves_balances_and_refunds_payer() {
    let mut mollusk = mollusk();
    let scenario = ObsScenario::transfer(
        GUARDIAN_SET_INDEX,
        0x44,
        Transfer::new(0, ETHEREUM, SOLANA, 500_000),
    );

    let after_one = scenario.submit_n(&mollusk, 1);
    let pending = find_account(&after_one, &scenario.pending_pda);
    assert_eq!(pending.owner, program_id());
    let mut expected = PendingObservationsLayout::new(
        scenario.chain,
        scenario.guardian_set_index,
        scenario.content_digest,
        SUBMITTER.to_bytes(),
        TX_ID,
    );
    expected.signatures = [0b1, 0, 0, 0];
    assert_eq!(pending_layout(pending), expected);
    assert_bucket_unmarked(find_account(&after_one, &scenario.noreplay_bucket));

    let after_twelve = scenario.submit_range(&mollusk, after_one, 1..12);
    let pending = find_account(&after_twelve, &scenario.pending_pda);
    assert_eq!(
        pending_layout(pending).signatures,
        [0b1111_1111_1111, 0, 0, 0]
    );
    assert_bucket_unmarked(find_account(&after_twelve, &scenario.noreplay_bucket));
    let pending_rent = pending.lamports;
    let alice_before = find_account(&after_twelve, &SUBMITTER).lamports;

    let bob = Pubkey::new_from_array([0xB0u8; 32]);
    let mut accounts = after_twelve.clone();
    accounts.push((bob, system_owned_account(50_000_000_000)));
    let mut metas = scenario.account_metas();
    metas[0] = AccountMeta::new(bob, true);
    metas[9] = AccountMeta::new(bob, false);
    let mismatch_logs = LogCollector::new_ref();
    mollusk.logger = Some(mismatch_logs.clone());
    let wrong_recipient = submit(
        &mollusk,
        accounts.clone(),
        scenario.ix_data(12),
        metas.clone(),
    );
    assert_error(
        &wrong_recipient,
        GlobalAccountantError::PayerMismatch as u64,
        "rent recipient must be the recorded payer",
    );
    assert_eq!(
        accpayr_logs(mismatch_logs.borrow().get_recorded_content()),
        [AccountantPayerLog::new(
            scenario.pending_pda.to_bytes(),
            SUBMITTER.to_bytes()
        )],
        "mismatch logs the recorded payer once"
    );

    metas[9] = AccountMeta::new(SUBMITTER, false);
    let commit_logs = LogCollector::new_ref();
    mollusk.logger = Some(commit_logs.clone());
    let committed = submit(&mollusk, accounts, scenario.ix_data(12), metas);
    assert_success(&committed, "quorum commit");
    assert!(
        accpayr_logs(commit_logs.borrow().get_recorded_content()).is_empty(),
        "matching recipient logs no payer"
    );
    mollusk.logger = None;
    let after = &committed.resulting_accounts;
    assert_closed(find_account(after, &scenario.pending_pda), "pending closed");
    assert_bucket_marked(
        find_account(after, &scenario.noreplay_bucket),
        scenario.sequence,
    );
    assert_eq!(
        find_account(after, &SUBMITTER).lamports,
        alice_before + pending_rent,
        "recorded payer refunded"
    );
    assert_eq!(
        balance_layout(find_account(after, &scenario.source_account)),
        BalanceAccountLayout::new(
            ETHEREUM,
            ETHEREUM,
            TOKEN_ADDRESS,
            Uint256::from_u128(500_000)
        )
    );
    assert_eq!(
        balance_layout(find_account(after, &scenario.dest_account)),
        BalanceAccountLayout::new(SOLANA, ETHEREUM, TOKEN_ADDRESS, Uint256::from_u128(500_000))
    );
}

/// Pending PDA oracle, independent of the program derivation: `tx_id` seeds as
/// `[len]`, `padded[..32]`, `padded[32..]`.
fn pending_pda_with_tx_id(s: &ObsScenario, tx_id: TxId<'_>) -> Pubkey {
    let id = tx_id.as_bytes();
    let mut padded = [0u8; 64];
    padded[..id.len()].copy_from_slice(id);
    let len = [u8::try_from(id.len()).expect("tx_id length fits u8")];
    Pubkey::find_program_address(
        &[
            b"pending",
            &s.chain.to_be_bytes(),
            &s.emitter,
            &s.sequence.to_be_bytes(),
            &s.guardian_set_index.to_be_bytes(),
            &s.content_digest,
            &len,
            &padded[..32],
            &padded[32..],
        ],
        &program_id(),
    )
    .0
}

fn submit_with_tx_id(
    mollusk: &Mollusk,
    s: &ObsScenario,
    accounts: Vec<(Pubkey, Account)>,
    pending: Pubkey,
    tx_id: TxId<'_>,
    guardian_index: u8,
) -> mollusk_svm::result::InstructionResult {
    let signature = sign_digest(
        &s.guardians[usize::from(guardian_index)],
        &signing_digest_with_tx_id(tx_id, &s.body),
    );
    let ix_data = submit_observations_ix_data_with_tx_id(
        s.guardian_set_index,
        guardian_index,
        signature,
        tx_id,
        &s.body,
    );
    let mut metas = s.account_metas();
    metas[1] = AccountMeta::new(pending, false);
    submit(mollusk, accounts, ix_data, metas)
}

/// `tx_id` seeds the pending PDA, matching wormchain's `(digest, tx_hash)` pending entries.
/// The first sibling at quorum commits. NoReplay rejects the other sibling, and
/// `close_pending` reclaims it.
#[test]
fn tx_id_splits_the_pending_pda_and_first_quorum_commits() {
    const CLOSER: Pubkey = Pubkey::new_from_array([0x22u8; 32]);
    let mollusk = mollusk();
    let s = ObsScenario::attest(GUARDIAN_COUNT, GUARDIAN_SET_INDEX, 0x51);
    let account_tx_id = TX_ID;
    let close_signature = [0x5Au8; 64];
    let close_tx_id = TxId::Signature(&close_signature);
    let account_pda = pending_pda_with_tx_id(&s, account_tx_id);
    let close_pda = pending_pda_with_tx_id(&s, close_tx_id);
    assert_ne!(account_pda, close_pda);

    let mut accounts = s.initial_accounts();
    accounts.push((account_pda, uninitialised_pda_account()));
    accounts.push((close_pda, uninitialised_pda_account()));
    accounts.push((CLOSER, system_owned_account(1_000_000_000)));

    let mismatch = submit_with_tx_id(&mollusk, &s, accounts.clone(), close_pda, account_tx_id, 0);
    assert_error(
        &mismatch,
        GlobalAccountantError::InvalidPda as u64,
        "account tx_id against the close sibling",
    );

    let first = submit_with_tx_id(&mollusk, &s, accounts, account_pda, account_tx_id, 0);
    assert_success(&first, "guardian 0, account tx_id");
    let mut accounts = first.resulting_accounts;
    assert_eq!(
        pending_layout(find_account(&accounts, &account_pda)).tx_id(),
        Ok(account_tx_id)
    );

    for i in 1..=QUORUM {
        let result = submit_with_tx_id(&mollusk, &s, accounts, close_pda, close_tx_id, i);
        assert_success(&result, &format!("guardian {i}, close tx_id"));
        accounts = result.resulting_accounts;
    }
    assert_closed(find_account(&accounts, &close_pda), "close sibling commits");
    assert_bucket_marked(find_account(&accounts, &s.noreplay_bucket), s.sequence);
    let stranded = find_account(&accounts, &account_pda);
    assert_eq!(stranded.owner, program_id());
    assert_eq!(pending_layout(stranded).num_signatures(), 1);

    let late = submit_with_tx_id(
        &mollusk,
        &s,
        accounts.clone(),
        account_pda,
        account_tx_id,
        1,
    );
    assert_error(
        &late,
        GlobalAccountantError::AlreadyAccounted as u64,
        "account sibling after the commit",
    );

    let stranded_lamports = find_account(&accounts, &account_pda).lamports;
    let payer_lamports = find_account(&accounts, &SUBMITTER).lamports;
    let close = mollusk.process_instruction(
        &Instruction::new_with_bytes(
            program_id(),
            &close_pending_ix_data(s.emitter, s.sequence),
            vec![
                AccountMeta::new(CLOSER, true),
                AccountMeta::new(account_pda, false),
                AccountMeta::new(SUBMITTER, false),
                AccountMeta::new_readonly(s.guardian_set, false),
                AccountMeta::new_readonly(s.noreplay_bucket, false),
            ],
        ),
        &accounts,
    );
    assert_success(&close, "close_pending reclaims the account sibling");
    assert_eq!(
        find_account(&close.resulting_accounts, &account_pda).lamports,
        0
    );
    assert_eq!(
        find_account(&close.resulting_accounts, &SUBMITTER).lamports,
        payer_lamports + stranded_lamports,
        "payer refunded"
    );
}

#[test]
fn routing_comes_from_body_not_caller_prefix() {
    let mollusk = mollusk();
    let scenario = ObsScenario::attest(GUARDIAN_COUNT, GUARDIAN_SET_INDEX, 0x80);
    let attacker_pending = accountant_operational_core::support::quorum::derive_pending_pda(
        &program_id(),
        99,
        &[0xFFu8; 32],
        0x9999,
        GUARDIAN_SET_INDEX,
        &scenario.content_digest,
        TX_ID,
    )
    .0;
    assert_ne!(attacker_pending, scenario.pending_pda);

    let mut accounts = scenario.initial_accounts();
    replace_account(
        &mut accounts,
        &scenario.pending_pda,
        uninitialised_pda_account(),
    );
    accounts[1].0 = attacker_pending;
    let mut metas = scenario.account_metas();
    metas[1] = AccountMeta::new(attacker_pending, false);

    let result = submit(&mollusk, accounts, scenario.ix_data(0), metas);
    assert_error(
        &result,
        GlobalAccountantError::InvalidPda as u64,
        "attacker pending address",
    );
    assert_eq!(
        find_account(&result.resulting_accounts, &attacker_pending).owner,
        system_program_id()
    );
}

#[test]
fn rejects() {
    let mut mollusk = mollusk();
    mollusk.sysvars.clock.unix_timestamp = NOW;
    type Case = fn(&Mollusk) -> (Vec<(Pubkey, Account)>, Vec<u8>, Vec<AccountMeta>);

    fn fresh(seed: u8) -> ObsScenario {
        ObsScenario::attest(GUARDIAN_COUNT, GUARDIAN_SET_INDEX, seed)
    }
    fn plain(
        s: ObsScenario,
        accounts: Vec<(Pubkey, Account)>,
        guardian_index: u8,
    ) -> (Vec<(Pubkey, Account)>, Vec<u8>, Vec<AccountMeta>) {
        (accounts, s.ix_data(guardian_index), s.account_metas())
    }
    fn signed(s: &ObsScenario, guardian_index: u8, signature: [u8; 65], body: &[u8]) -> Vec<u8> {
        submit_observations_ix_data(s.guardian_set_index, guardian_index, signature, body)
    }

    let cases: [(&str, Case, u64); 21] = [
        (
            "corrupted signature",
            |_| {
                let s = fresh(0x45);
                let mut sig = sign_digest(&s.guardians[0], &s.signing_digest);
                sig[0] ^= 0xff;
                let ix = signed(&s, 0, sig, &s.body);
                let accounts = s.initial_accounts();
                let metas = s.account_metas();
                (accounts, ix, metas)
            },
            GlobalAccountantError::InvalidSignature as u64,
        ),
        (
            "recovery id 4",
            |_| {
                let s = fresh(0x52);
                let mut sig = sign_digest(&s.guardians[0], &s.signing_digest);
                sig[64] = 4;
                let ix = signed(&s, 0, sig, &s.body);
                let accounts = s.initial_accounts();
                let metas = s.account_metas();
                (accounts, ix, metas)
            },
            GlobalAccountantError::InvalidSignature as u64,
        ),
        (
            "tampered body",
            |_| {
                let s = fresh(0x64);
                let sig = sign_digest(&s.guardians[0], &s.signing_digest);
                let mut body = s.body.clone();
                body[0] ^= 0xAA;
                let ix = signed(&s, 0, sig, &body);
                let accounts = s.initial_accounts();
                let metas = s.account_metas();
                (accounts, ix, metas)
            },
            GlobalAccountantError::InvalidSignature as u64,
        ),
        (
            "legacy bare-digest signature",
            |_| {
                let s = fresh(0x71);
                assert_ne!(s.content_digest, s.signing_digest);
                let sig = sign_digest(&s.guardians[0], &s.content_digest);
                let ix = signed(&s, 0, sig, &s.body);
                let accounts = s.initial_accounts();
                let metas = s.account_metas();
                (accounts, ix, metas)
            },
            GlobalAccountantError::InvalidSignature as u64,
        ),
        (
            "duplicate guardian index",
            |m| {
                let s = fresh(0x46);
                let accounts = s.submit_n(m, 1);
                plain(s, accounts, 0)
            },
            GlobalAccountantError::AlreadySigned as u64,
        ),
        (
            "guardian set truncated header",
            |_| {
                let s = fresh(0x53);
                let mut accounts = s.initial_accounts();
                replace_account(
                    &mut accounts,
                    &s.guardian_set,
                    Account {
                        lamports: 1_000_000,
                        data: vec![0u8; 4],
                        owner: core_bridge_program_id(),
                        executable: false,
                        rent_epoch: 0,
                    },
                );
                plain(s, accounts, 0)
            },
            GlobalAccountantError::InvalidPda as u64,
        ),
        (
            "guardian set index mismatch",
            |_| {
                let s = fresh(0x53);
                let mut accounts = s.initial_accounts();
                replace_account(
                    &mut accounts,
                    &s.guardian_set,
                    guardian_set_with(99, &s.guardians, &core_bridge_program_id()),
                );
                plain(s, accounts, 0)
            },
            GlobalAccountantError::InvalidPda as u64,
        ),
        (
            "guardian index beyond set",
            |_| {
                let s = fresh(0x53);
                let mut accounts = s.initial_accounts();
                replace_account(
                    &mut accounts,
                    &s.guardian_set,
                    guardian_set_with(
                        GUARDIAN_SET_INDEX,
                        &s.guardians[..3],
                        &core_bridge_program_id(),
                    ),
                );
                plain(s, accounts, 5)
            },
            GlobalAccountantError::InvalidGuardianIndex as u64,
        ),
        (
            "guardian set keys truncated",
            |_| {
                let s = fresh(0x53);
                let mut data = Vec::new();
                data.extend_from_slice(&GUARDIAN_SET_INDEX.to_le_bytes());
                data.extend_from_slice(&19u32.to_le_bytes());
                data.extend_from_slice(&[0u8; 5 * 20]);
                let mut accounts = s.initial_accounts();
                replace_account(
                    &mut accounts,
                    &s.guardian_set,
                    Account {
                        lamports: 1_000_000,
                        data,
                        owner: core_bridge_program_id(),
                        executable: false,
                        rent_epoch: 0,
                    },
                );
                plain(s, accounts, 18)
            },
            GlobalAccountantError::InvalidPda as u64,
        ),
        (
            "guardian set not owned by core bridge",
            |_| {
                let s = fresh(0x66);
                let mut accounts = s.initial_accounts();
                replace_account(
                    &mut accounts,
                    &s.guardian_set,
                    guardian_set_with(GUARDIAN_SET_INDEX, &s.guardians, &system_program_id()),
                );
                plain(s, accounts, 0)
            },
            GlobalAccountantError::InvalidPda as u64,
        ),
        (
            "expired guardian set",
            |_| {
                let s = fresh(0x47);
                let mut accounts = s.initial_accounts();
                replace_account(
                    &mut accounts,
                    &s.guardian_set,
                    guardian_set_account(
                        GUARDIAN_SET_INDEX,
                        &guardian_keys(&s.guardians),
                        0,
                        NOW as u32 - 1,
                        &core_bridge_program_id(),
                    ),
                );
                plain(s, accounts, 0)
            },
            GlobalAccountantError::ExpiredGuardianSet as u64,
        ),
        (
            // Mainnet set 0: one key, `expiration_time == 0`, retired by creation time only.
            "legacy mainnet guardian set 0",
            |_| {
                const MAINNET_GENESIS_SET_CREATION_TIME: u32 = 1_628_099_186;
                let s = ObsScenario::attest(1, 0, 0x50);
                let mut accounts = s.initial_accounts();
                replace_account(
                    &mut accounts,
                    &s.guardian_set,
                    guardian_set_account(
                        0,
                        &guardian_keys(&s.guardians),
                        MAINNET_GENESIS_SET_CREATION_TIME,
                        0,
                        &core_bridge_program_id(),
                    ),
                );
                plain(s, accounts, 0)
            },
            GlobalAccountantError::ExpiredGuardianSet as u64,
        ),
        (
            "guardian set larger than the 128-bit bitmap",
            |_| {
                let s = ObsScenario::attest(129, GUARDIAN_SET_INDEX, 0x51);
                let accounts = s.initial_accounts();
                plain(s, accounts, 0)
            },
            GlobalAccountantError::GuardianSetTooLarge as u64,
        ),
        (
            "superseded guardian set inside expiry window accepted",
            |_| {
                let s = fresh(0x49);
                let mut accounts = s.initial_accounts();
                replace_account(
                    &mut accounts,
                    &s.guardian_set,
                    guardian_set_account(
                        GUARDIAN_SET_INDEX,
                        &guardian_keys(&s.guardians),
                        0,
                        NOW as u32 + 100,
                        &core_bridge_program_id(),
                    ),
                );
                plain(s, accounts, 0)
            },
            u64::MAX,
        ),
        (
            "pre-marked noreplay",
            |_| {
                let s = fresh(0x4D);
                let mut accounts = s.initial_accounts();
                replace_account(
                    &mut accounts,
                    &s.noreplay_bucket,
                    noreplay_bucket_marked(s.sequence),
                );
                plain(s, accounts, 0)
            },
            GlobalAccountantError::AlreadyAccounted as u64,
        ),
        (
            // The real bucket is marked. A bucket derived from a fake authority is empty, so
            // a pre-check keyed on the caller's authority would pass and open a pending PDA.
            "fake noreplay authority",
            |_| {
                let s = fresh(0x4E);
                let fake_authority = Pubkey::new_unique();
                let fake_bucket =
                    derive_bucket_pda(&fake_authority, s.chain, &s.emitter, s.sequence).0;
                let mut accounts = s.initial_accounts();
                replace_account(
                    &mut accounts,
                    &s.noreplay_bucket,
                    noreplay_bucket_marked(s.sequence),
                );
                accounts.push((fake_authority, system_owned_account(0)));
                accounts.push((fake_bucket, noreplay_bucket_unmarked()));
                let mut metas = s.account_metas();
                metas[3] = AccountMeta::new(fake_bucket, false);
                metas[6] = AccountMeta::new_readonly(fake_authority, false);
                (accounts, s.ix_data(0), metas)
            },
            GlobalAccountantError::InvalidPda as u64,
        ),
        (
            "missing chain registration",
            |_| {
                let s = fresh(0xA0);
                let mut accounts = s.initial_accounts();
                replace_account(
                    &mut accounts,
                    &s.chain_registration,
                    uninitialised_pda_account(),
                );
                plain(s, accounts, 0)
            },
            GlobalAccountantError::MissingChainRegistration as u64,
        ),
        (
            "unregistered emitter",
            |_| {
                let s = fresh(0xA1);
                let mut accounts = s.initial_accounts();
                replace_account(
                    &mut accounts,
                    &s.chain_registration,
                    chain_registration_account(s.chain, [0xCC; 32]),
                );
                plain(s, accounts, 0)
            },
            GlobalAccountantError::UnregisteredEmitter as u64,
        ),
        (
            "spoofed registration pda",
            |_| {
                let s = fresh(0xA2);
                let spoofed =
                    accountant_operational_core::accounts::chain_registration::derive_pda(
                        &program_id(),
                        99,
                    )
                    .0;
                let mut accounts = s.initial_accounts();
                accounts.push((spoofed, chain_registration_account(99, [0xAA; 32])));
                let ix = s.ix_data(0);
                let mut metas = s.account_metas();
                metas[10] = AccountMeta::new_readonly(spoofed, false);
                (accounts, ix, metas)
            },
            GlobalAccountantError::InvalidPda as u64,
        ),
        (
            "non-canonical pending pda on continue",
            |m| {
                let s = fresh(0x67);
                let mut accounts = s.submit_n(m, 1);
                let spoofed = Pubkey::new_unique();
                let pending = find_account(&accounts, &s.pending_pda).clone();
                accounts.push((spoofed, pending));
                let ix = s.ix_data(1);
                let mut metas = s.account_metas();
                metas[1] = AccountMeta::new(spoofed, false);
                (accounts, ix, metas)
            },
            GlobalAccountantError::InvalidPda as u64,
        ),
        (
            "wrong source pda at quorum",
            |m| {
                let mut s = ObsScenario::transfer(
                    GUARDIAN_SET_INDEX,
                    0x65,
                    Transfer::new(0, ETHEREUM, SOLANA, 100),
                );
                s.source_account =
                    balance::derive_pda(&program_id(), ETHEREUM, 99, &TOKEN_ADDRESS).0;
                let accounts = s.submit_n(m, 12);
                plain(s, accounts, 12)
            },
            GlobalAccountantError::InvalidAccountPda as u64,
        ),
    ];

    for (label, case, expected) in cases {
        let (accounts, ix_data, metas) = case(&mollusk);
        let before = accounts.clone();
        let result = submit(&mollusk, accounts, ix_data, metas);
        // `u64::MAX` marks the one positive control in this table.
        if expected == u64::MAX {
            assert_success(&result, label);
            continue;
        }
        assert_error(&result, expected, label);
        assert_eq!(
            result.resulting_accounts, before,
            "{label}: state untouched"
        );
    }
}

/// A newer guardian set opens a sibling pending PDA keyed on its index. The older set's
/// record, bitmap, and rent are untouched, matching wormchain's per-set pending entries.
#[test]
fn guardian_set_rotation_opens_sibling_pending_and_tracks_live_size() {
    let mollusk = mollusk();

    let old = ObsScenario::attest(GUARDIAN_COUNT, 4, 0x4A);
    let mut accounts = old.submit_n(&mollusk, 1);
    let old_pending = find_account(&accounts, &old.pending_pda).clone();
    let new_guardians = make_guardians(GUARDIAN_COUNT, 0x4B);
    let new_set = derive_guardian_set_pda(5, &core_bridge_program_id()).0;
    accounts.push((
        new_set,
        guardian_set_with(5, &new_guardians, &core_bridge_program_id()),
    ));
    let sibling = accountant_operational_core::support::quorum::derive_pending_pda(
        &program_id(),
        old.chain,
        &old.emitter,
        old.sequence,
        5,
        &old.content_digest,
        TX_ID,
    )
    .0;
    assert_ne!(sibling, old.pending_pda);
    accounts.push((sibling, uninitialised_pda_account()));
    let sig = sign_digest(&new_guardians[0], &old.signing_digest);
    let mut metas = old.account_metas();
    metas[1] = AccountMeta::new(sibling, false);
    metas[2] = AccountMeta::new_readonly(new_set, false);
    let rotated = submit(
        &mollusk,
        accounts,
        submit_observations_ix_data(5, 0, sig, &old.body),
        metas,
    );
    assert_success(&rotated, "observation under new set");
    let after = &rotated.resulting_accounts;
    assert_eq!(
        *find_account(after, &old.pending_pda),
        old_pending,
        "old set record untouched"
    );
    let pending = pending_layout(find_account(after, &sibling));
    assert_eq!(pending.guardian_set_index, 5);
    assert_eq!(pending.signatures, [0b1, 0, 0, 0]);

    let small = ObsScenario::attest(6, GUARDIAN_SET_INDEX, 0x9A);
    let quorum = PendingObservationsLayout::quorum_for(6) as u8;
    assert_eq!(quorum, 5);
    let accounts = small.submit_n(&mollusk, quorum - 1);
    assert_bucket_unmarked(find_account(&accounts, &small.noreplay_bucket));
    let result = small.submit_once(&mollusk, accounts, quorum - 1);
    assert_success(&result, "quorum of 5 of 6");
    assert_bucket_marked(
        find_account(&result.resulting_accounts, &small.noreplay_bucket),
        small.sequence,
    );
}

/// Quorum of a 33-key set is 23. Indices 10..=32 reach it; index 32 needs the second
/// bitmap word, matching wormchain's 128-bit `Data.signatures`.
#[test]
fn guardian_index_32_counts_toward_quorum_in_a_33_key_set() {
    let mollusk = mollusk();
    let s = ObsScenario::attest(33, GUARDIAN_SET_INDEX, 0x60);
    let quorum = PendingObservationsLayout::quorum_for(33) as u8;
    assert_eq!(quorum, 23);

    let accounts = s.submit_range(&mollusk, s.initial_accounts(), 10..33);
    assert_bucket_marked(find_account(&accounts, &s.noreplay_bucket), s.sequence);
    assert_closed(
        find_account(&accounts, &s.pending_pda),
        "pending closed at quorum",
    );
}

#[test]
fn fork_observations_accumulate_in_sibling_pendings() {
    let mollusk = mollusk();
    let d1 = ObsScenario::attest(GUARDIAN_COUNT, 6, 0x5A);
    let mut accounts = d1.submit_n(&mollusk, 7);
    assert_eq!(
        pending_layout(find_account(&accounts, &d1.pending_pda)).num_signatures(),
        7
    );

    let mut d2 = d1.clone();
    let mut body = d1.body.clone();
    body[50] = 0xA5;
    d2.set_body(body);
    assert_ne!(d2.content_digest, d1.content_digest);
    assert_ne!(d2.pending_pda, d1.pending_pda);
    accounts.push((d2.pending_pda, uninitialised_pda_account()));

    let first = d2.submit_once(&mollusk, accounts.clone(), 1);
    assert_success(&first, "first sibling observation");
    let sibling = pending_layout(find_account(&first.resulting_accounts, &d2.pending_pda));
    assert_eq!(sibling.content_digest, d2.content_digest);
    assert_eq!(sibling.signatures, [0b10, 0, 0, 0]);
    assert_eq!(sibling.guardian_set_index, d2.guardian_set_index);
    assert_eq!(
        pending_layout(find_account(&first.resulting_accounts, &d1.pending_pda)).num_signatures(),
        7
    );

    let accounts = d2.submit_range(&mollusk, accounts, 0..QUORUM);
    assert_bucket_marked(find_account(&accounts, &d2.noreplay_bucket), d2.sequence);
    assert_closed(find_account(&accounts, &d2.pending_pda), "sibling closed");
    let stranded = find_account(&accounts, &d1.pending_pda);
    assert_eq!(stranded.owner, program_id());
    assert_eq!(pending_layout(stranded).num_signatures(), 7);
    assert_eq!(pending_layout(stranded).content_digest, d1.content_digest);
}

#[test]
fn attest_and_unknown_payload_leave_balances_and_replay_slot() {
    let mollusk = mollusk();

    let attest = ObsScenario::attest(GUARDIAN_COUNT, GUARDIAN_SET_INDEX, 0x63);
    let accounts = attest.submit_n(&mollusk, QUORUM);
    assert_bucket_marked(
        find_account(&accounts, &attest.noreplay_bucket),
        attest.sequence,
    );
    let sentinel = find_account(&accounts, &noreplay_authority());
    assert_eq!(sentinel.owner, system_program_id());
    assert!(sentinel.data.is_empty());

    let mut unknown = ObsScenario::attest(GUARDIAN_COUNT, GUARDIAN_SET_INDEX, 0x65);
    let mut body = unknown.body.clone();
    body[51] = 0x05;
    unknown.set_body(body);
    let accounts = unknown.submit_n(&mollusk, 12);
    let result = unknown.submit_once(&mollusk, accounts, 12);
    assert_error(
        &result,
        GlobalAccountantError::UnknownTokenBridgePayload as u64,
        "unknown payload at quorum",
    );
    assert_bucket_unmarked(find_account(
        &result.resulting_accounts,
        &unknown.noreplay_bucket,
    ));
    let pending = find_account(&result.resulting_accounts, &unknown.pending_pda);
    assert_eq!(pending.owner, program_id());
    assert_eq!(pending_layout(pending).num_signatures(), 12);
}

#[test]
fn wrapped_source_underflow_rejects_at_quorum() {
    let mollusk = mollusk();
    let scenario = ObsScenario::transfer(
        GUARDIAN_SET_INDEX,
        0x61,
        Transfer::new(0, SOLANA, ETHEREUM, 1_000),
    );
    let mut accounts = scenario.submit_n(&mollusk, 12);
    for (key, chain) in [
        (scenario.source_account, SOLANA),
        (scenario.dest_account, ETHEREUM),
    ] {
        replace_account(
            &mut accounts,
            &key,
            balance_account(chain, ETHEREUM, TOKEN_ADDRESS, Uint256::from_u128(999)),
        );
    }
    let result = scenario.submit_once(&mollusk, accounts, 12);
    assert_error(
        &result,
        GlobalAccountantError::BalanceUnderflow as u64,
        "wrapped source underflow",
    );
    assert_bucket_unmarked(find_account(
        &result.resulting_accounts,
        &scenario.noreplay_bucket,
    ));
}

#[test]
fn dusted_dest_pda_still_initialises() {
    let mollusk = mollusk();
    let scenario = ObsScenario::transfer(
        GUARDIAN_SET_INDEX,
        0x63,
        Transfer::new(0, ETHEREUM, SOLANA, 7_777),
    );
    let mut accounts = scenario.initial_accounts();
    replace_account(
        &mut accounts,
        &scenario.dest_account,
        system_owned_account(1),
    );
    let accounts = scenario.submit_range(&mollusk, accounts, 0..QUORUM);
    let dest = find_account(&accounts, &scenario.dest_account);
    assert_eq!(dest.owner, program_id());
    assert!(dest.lamports > 1);
    assert_eq!(balance_layout(dest).balance, Uint256::from_u128(7_777));
}

#[test]
fn quorum_branch_cu_below_ceiling() {
    let mollusk = mollusk();
    let scenario = ObsScenario::transfer(
        GUARDIAN_SET_INDEX,
        0xCF,
        Transfer::new(0, ETHEREUM, SOLANA, 12_345),
    );
    let accounts = scenario.submit_n(&mollusk, QUORUM - 1);
    let result = scenario.submit_once(&mollusk, accounts, QUORUM - 1);
    assert_success(&result, "quorum");
    assert!(
        result.compute_units_consumed <= MAX_QUORUM_BRANCH_CU,
        "quorum branch used {} CU, ceiling {}",
        result.compute_units_consumed,
        MAX_QUORUM_BRANCH_CU
    );
}

const _: () =
    assert!(QUORUM as u32 == PendingObservationsLayout::quorum_for(GUARDIAN_COUNT as u32));

#[test]
fn submit_observations_rejects_cpi_invocation() {
    let forwarder = Pubkey::new_from_array([0xF0u8; 32]);
    let mut mollusk = mollusk();
    mollusk.add_program(&forwarder, "test_cpi_forwarder");
    let scenario = ObsScenario::transfer(
        GUARDIAN_SET_INDEX,
        0x51,
        Transfer::new(0, ETHEREUM, SOLANA, 500_000),
    );

    let mut metas = vec![AccountMeta::new_readonly(program_id(), false)];
    metas.extend(scenario.account_metas());
    let mut accounts = scenario.initial_accounts();
    accounts.push((
        program_id(),
        create_program_account_loader_v3(&program_id()),
    ));
    let ix = Instruction::new_with_bytes(forwarder, &scenario.ix_data(0), metas);
    let result = mollusk.process_instruction(&ix, &accounts);

    assert_error(
        &result,
        GlobalAccountantError::CpiInvocation as u64,
        "submit_observations through CPI",
    );
}
