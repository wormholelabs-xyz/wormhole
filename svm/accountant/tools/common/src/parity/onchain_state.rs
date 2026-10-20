//! Actual side of the comparison: the state read from Solana.
//!
//! Program accounts come from one unfiltered `getProgramAccounts`, so a
//! sample is one view and accounts of every tag are seen. Each account is
//! parsed through its definitions layout, re-encoded with `Layout::new`, and
//! must equal the original bytes and sit at the PDA its own key derives.
//! An all-zero account is a foreign account, logged only. Anything else is a
//! [`FindingKind::MalformedAccounts`] finding.
//!
//! NoReplay bucket PDAs are one-way, so they cannot be listed; coverage is the
//! `(chain, emitter, bucket)` universe of the expected replay set, fetched with
//! batched `getMultipleAccounts`. Every set bit of a fetched bucket is read
//! back, so an extra mark inside that universe is a finding.
//!
//! [`MAX_PROGRAM_ACCOUNTS`] bounds processing. The whole `getProgramAccounts`
//! response is in memory when the bound is checked.

use anyhow::{bail, Context, Result};
use global_accountant_definitions::{
    AccountLayout, AccountTag, BalanceAccountLayout, ChainRegistrationLayout, ModifyBalanceLayout,
    PendingObservationsLayout, RegisterChainLayout, TransceiverHubLayout, TransceiverPeerLayout,
};
use solana_client::nonblocking::rpc_client::RpcClient;
use solana_client::rpc_config::{
    RpcAccountInfoConfig, RpcProgramAccountsConfig, UiAccountEncoding,
};
use solana_pubkey::Pubkey;

use crate::accountant::Accountant;
use crate::ids::BucketId;
use crate::noreplay::{derive_noreplay_authority_pda, fetch_bucket_states, BucketState};
use crate::parity::compare::Finding;
use crate::parity::compared_account::ComparedAccount;
use crate::parity::state::{ChainState, FindingKind, ReplayMarks};

/// Program accounts per sample. Mainnet: 17,367 balances plus tens of others,
/// plus live `PendingObservations` after the cutover.
pub const MAX_PROGRAM_ACCOUNTS: usize = 2_000_000;

/// Foreign account addresses printed per sample; the count is always printed.
pub const MAX_LOGGED_FOREIGN_ACCOUNTS: usize = 100;

/// Findings from Solana alone: malformed program accounts, the pending count,
/// and foreign accounts.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SolanaOnlyFindings {
    pub malformed: Vec<Finding>,
    pub pending_observations: usize,
    /// All-zero program accounts. Bounded by [`MAX_PROGRAM_ACCOUNTS`].
    pub foreign_accounts: Vec<Pubkey>,
}

impl SolanaOnlyFindings {
    /// Print the foreign account count and up to [`MAX_LOGGED_FOREIGN_ACCOUNTS`] addresses.
    pub fn log_foreign_accounts(&self) {
        let count = self.foreign_accounts.len();
        if count == 0 {
            return;
        }
        eprintln!(
            "[parity] warning: {count} foreign account(s): all-zero, owned by the program; \
             not findings"
        );
        for address in self
            .foreign_accounts
            .iter()
            .take(MAX_LOGGED_FOREIGN_ACCOUNTS)
        {
            eprintln!("[parity]   foreign {address}");
        }
        if count > MAX_LOGGED_FOREIGN_ACCOUNTS {
            eprintln!(
                "[parity]   ... {} more",
                count - MAX_LOGGED_FOREIGN_ACCOUNTS
            );
        }
    }
}

/// One sample of the program's state.
pub async fn fetch(
    rpc: &RpcClient,
    program_id: &Pubkey,
    noreplay_program: &Pubkey,
    accountant: Accountant,
    replay_universe: &ReplayMarks,
) -> Result<(ChainState, SolanaOnlyFindings)> {
    let mut state = ChainState::default();
    let mut solana_only = SolanaOnlyFindings::default();

    #[allow(deprecated)] // get_program_ui_accounts returns UiAccount which is harder to byte-parse
    let accounts = rpc
        .get_program_accounts_with_config(program_id, base64_config())
        .await
        .context("getProgramAccounts")?;
    if accounts.len() > MAX_PROGRAM_ACCOUNTS {
        bail!(
            "{} program accounts exceed MAX_PROGRAM_ACCOUNTS ({MAX_PROGRAM_ACCOUNTS})",
            accounts.len()
        );
    }
    for (address, account) in &accounts {
        if account.owner != *program_id {
            bail!(
                "getProgramAccounts returned {address} owned by {}",
                account.owner
            );
        }
        classify_account(
            &mut state,
            &mut solana_only,
            program_id,
            accountant,
            address,
            &account.data,
        )?;
    }

    fetch_replay_marks(
        rpc,
        program_id,
        noreplay_program,
        replay_universe,
        &mut state,
        &mut solana_only,
    )
    .await?;
    Ok((state, solana_only))
}

/// Base64 explicitly: base58, the RPC default, fails on accounts over 128 bytes.
fn base64_config() -> RpcProgramAccountsConfig {
    RpcProgramAccountsConfig {
        account_config: RpcAccountInfoConfig {
            encoding: Some(UiAccountEncoding::Base64),
            ..RpcAccountInfoConfig::default()
        },
        ..RpcProgramAccountsConfig::default()
    }
}

fn malformed_account(address: &Pubkey, description: String) -> Finding {
    Finding {
        kind: FindingKind::MalformedAccounts,
        key: address.to_string(),
        expected: None,
        actual: Some(description),
    }
}

/// Every account tag, for decoding the tag byte.
const TAGS: [AccountTag; 7] = [
    AccountTag::PendingObservations,
    AccountTag::Balance,
    AccountTag::ChainRegistration,
    AccountTag::ModifyBalance,
    AccountTag::RegisterChain,
    AccountTag::TransceiverHub,
    AccountTag::TransceiverPeer,
];

fn decode_account_tag(byte: u8) -> Option<AccountTag> {
    TAGS.into_iter().find(|tag| *tag as u8 == byte)
}

/// SECURITY: only the owner writes account data, and every program write sets a
/// non-zero tag in the instruction that creates the account. An all-zero account
/// therefore comes from the System program (`CreateAccount`, `*WithSeed`, `Assign`)
/// or another program's PDA, at any address on or off the curve. Anyone can make
/// one, so it must not fail a check. A record PDA left unwritten still fails the
/// state comparison as a missing key.
fn is_foreign_account(data: &[u8]) -> bool {
    data.iter().all(|byte| *byte == 0)
}

/// Put one program account into its keyed state, count it as pending, record
/// it as foreign, or record it as malformed. `Err` is a fatal state error
/// (duplicate, cap).
pub fn classify_account(
    state: &mut ChainState,
    solana_only: &mut SolanaOnlyFindings,
    program_id: &Pubkey,
    accountant: Accountant,
    address: &Pubkey,
    data: &[u8],
) -> Result<()> {
    if is_foreign_account(data) {
        solana_only.foreign_accounts.push(*address);
        return Ok(());
    }
    let malformed_before = solana_only.malformed.len();
    let description =
        match decode_layout(state, solana_only, program_id, accountant, address, data)? {
            Some(description) => description,
            None => return Ok(()),
        };
    solana_only
        .malformed
        .push(malformed_account(address, description));
    assert_eq!(solana_only.malformed.len(), malformed_before + 1);
    Ok(())
}

/// `Ok(Some(_))` describes an account that fits no layout.
fn decode_layout(
    state: &mut ChainState,
    solana_only: &mut SolanaOnlyFindings,
    program_id: &Pubkey,
    accountant: Accountant,
    address: &Pubkey,
    data: &[u8],
) -> Result<Option<String>> {
    let tag_byte = *data.first().expect("an empty account is foreign");
    let insert_as = match decode_account_tag(tag_byte) {
        Some(AccountTag::PendingObservations) => {
            if data.len() != PendingObservationsLayout::LEN {
                return Ok(Some(format!(
                    "PendingObservations tag with {} bytes",
                    data.len()
                )));
            }
            solana_only.pending_observations += 1;
            return Ok(None);
        }
        Some(AccountTag::Balance) => check_and_insert::<BalanceAccountLayout>,
        Some(AccountTag::ModifyBalance) => check_and_insert::<ModifyBalanceLayout>,
        Some(AccountTag::ChainRegistration) => check_and_insert::<ChainRegistrationLayout>,
        Some(AccountTag::RegisterChain) => check_and_insert::<RegisterChainLayout>,
        Some(AccountTag::TransceiverHub) if accountant == Accountant::Ntt => {
            check_and_insert::<TransceiverHubLayout>
        }
        Some(AccountTag::TransceiverPeer) if accountant == Accountant::Ntt => {
            check_and_insert::<TransceiverPeerLayout>
        }
        Some(AccountTag::TransceiverHub | AccountTag::TransceiverPeer) | None => {
            return Ok(Some(format!(
                "tag {tag_byte} with {} bytes is not {accountant} state",
                data.len()
            )));
        }
    };
    insert_as(state, program_id, address, data)
}

/// Parse `data` as `L`, require the canonical encoding at the canonical
/// address, and add it to `L`'s keyed state.
fn check_and_insert<L: ComparedAccount>(
    state: &mut ChainState,
    program_id: &Pubkey,
    address: &Pubkey,
    data: &[u8],
) -> Result<Option<String>> {
    let tag = data[0];
    if data.len() != <L as AccountLayout>::LEN {
        return Ok(Some(format!(
            "tag {tag} with {} bytes, want {}",
            data.len(),
            <L as AccountLayout>::LEN
        )));
    }
    let layout: L = bytemuck::pod_read_unaligned(data);
    assert_eq!(
        layout.tag(),
        <L as AccountLayout>::TAG,
        "dispatched on the tag byte"
    );
    let Some(reencoded) = layout.reencode() else {
        return Ok(Some(format!("tag {tag} with an invalid field")));
    };
    if bytemuck::bytes_of(&reencoded) != data {
        return Ok(Some(format!(
            "tag {tag} with non-canonical bytes 0x{}",
            hex::encode(data)
        )));
    }
    let derived = layout.address(program_id);
    if derived != *address {
        return Ok(Some(format!(
            "tag {tag} at a non-canonical address; its key derives {derived}"
        )));
    }
    state.insert_account(&layout)?;
    Ok(None)
}

/// Read the buckets of `universe` and store their marks.
async fn fetch_replay_marks(
    rpc: &RpcClient,
    program_id: &Pubkey,
    noreplay_program: &Pubkey,
    universe: &ReplayMarks,
    state: &mut ChainState,
    solana_only: &mut SolanaOnlyFindings,
) -> Result<()> {
    let authority = derive_noreplay_authority_pda(program_id);
    let buckets: Vec<BucketId> = universe.buckets().keys().copied().collect();
    let fetched = fetch_bucket_states(rpc, noreplay_program, &authority, &buckets).await?;
    for (bucket, (pda, bucket_state)) in buckets.into_iter().zip(fetched) {
        let description = match bucket_state {
            BucketState::Missing => continue,
            BucketState::Bitmap(bitmap) => {
                state.replay_marks.insert_bucket(bucket, bitmap.bitmap)?;
                continue;
            }
            BucketState::Foreign(owner) => format!("bucket owned by {owner}"),
            BucketState::Malformed(len) => format!("bucket with {len} bytes"),
        };
        solana_only
            .malformed
            .push(malformed_account(&pda, description));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::{Address, ChainId, Sequence};
    use crate::layouts::{
        balance_layout, chain_registration_layout, modify_balance_layout, register_chain_layout,
        transceiver_hub_layout, transceiver_peer_layout, PdaAccount,
    };
    use crate::parity::state::{KeyedKind, StateKind};
    use crate::snapshot::{
        BalanceRow, ChainRegistrationRow, ModificationKind, ModifyBalanceRow, TransceiverHubRow,
        TransceiverPeerRow,
    };
    use global_accountant_definitions::Uint256;

    fn program() -> Pubkey {
        Pubkey::new_from_array([7; 32])
    }

    /// `(account bytes, its canonical address)`.
    fn account<L: PdaAccount>(layout: L) -> (Vec<u8>, Pubkey) {
        (
            bytemuck::bytes_of(&layout).to_vec(),
            layout.address(&program()),
        )
    }

    fn balance() -> (Vec<u8>, Pubkey) {
        account(balance_layout(&BalanceRow {
            chain: ChainId(1),
            token_chain: ChainId(2),
            token_address: Address([0xAA; 32]),
            balance: Uint256::from_u128(9),
        }))
    }

    fn registration() -> ChainRegistrationRow {
        ChainRegistrationRow {
            chain: ChainId(4),
            registered_emitter: Address([0xCC; 32]),
            sequence: Sequence(11),
        }
    }

    fn hub() -> (Vec<u8>, Pubkey) {
        account(transceiver_hub_layout(&TransceiverHubRow {
            chain: ChainId(5),
            address: Address([0x01; 32]),
            hub_chain: ChainId(6),
            hub_address: Address([0x02; 32]),
        }))
    }

    /// An off-curve address: a PDA of the program.
    fn pda() -> Pubkey {
        let (pda, _) = Pubkey::find_program_address(&[b"unrelated"], &program());
        assert!(!pda.is_on_curve());
        pda
    }

    /// An on-curve address: the compressed ed25519 base point, as a keypair address.
    fn keypair_address() -> Pubkey {
        let mut bytes = [0x66; 32];
        bytes[0] = 0x58;
        let address = Pubkey::new_from_array(bytes);
        assert!(address.is_on_curve());
        address
    }

    /// An off-curve `CreateAccountWithSeed` address owned by the program:
    /// `sha256(base ‖ seed ‖ owner)`, signed for by `base` alone.
    fn off_curve_seed_address() -> Pubkey {
        use sha2::{Digest, Sha256};
        const MAX_SEED_ATTEMPTS: u32 = 256;
        let base = keypair_address();
        for attempt in 0..MAX_SEED_ATTEMPTS {
            let mut hasher = Sha256::new();
            hasher.update(base.as_ref());
            hasher.update(attempt.to_string().as_bytes());
            hasher.update(program().as_ref());
            let address = Pubkey::new_from_array(hasher.finalize().into());
            if !address.is_on_curve() {
                return address;
            }
        }
        panic!("no off-curve seed address in {MAX_SEED_ATTEMPTS} attempts");
    }

    /// `(name, accountant, (account bytes, address), expected outcome)`.
    type Case = (&'static str, Accountant, (Vec<u8>, Pubkey), Want);

    enum Want {
        Kept(StateKind),
        Pending,
        Foreign,
        Malformed(&'static str),
    }

    #[test]
    fn classify_account_table() {
        let modify = account(modify_balance_layout(&ModifyBalanceRow {
            sequence: Sequence(3),
            chain_id: ChainId(1),
            token_chain: ChainId(2),
            token_address: Address([0xAB; 32]),
            amount: Uint256::from_u128(4),
            reason: [0; 32],
            modify_kind: ModificationKind::Add,
        }));
        let peer = account(transceiver_peer_layout(&TransceiverPeerRow {
            chain: ChainId(5),
            address: Address([0x01; 32]),
            dest_chain: ChainId(7),
            peer_address: Address([0x03; 32]),
        }));
        let pending_tag = AccountTag::PendingObservations as u8;
        let mut pending = vec![0u8; PendingObservationsLayout::LEN];
        pending[0] = pending_tag;
        let mut modify_bad_kind = modify.clone();
        // `kind` byte follows the tag; 0xFF is no ModificationKind.
        modify_bad_kind.0[1] = 0xFF;
        let mut balance_padding = balance();
        // `_pad0` follows the tag; the constructor writes 0.
        balance_padding.0[1] = 0xFF;
        let (balance_bytes, _) = balance();

        let cases: Vec<Case> = vec![
            (
                "balance",
                Accountant::Wtt,
                balance(),
                Want::Kept(StateKind::BALANCES),
            ),
            (
                "modify balance",
                Accountant::Wtt,
                modify.clone(),
                Want::Kept(StateKind::Keyed(KeyedKind::ModifyBalanceRecords)),
            ),
            (
                "chain registration",
                Accountant::Wtt,
                account(chain_registration_layout(&registration())),
                Want::Kept(StateKind::Keyed(KeyedKind::ChainRegistrations)),
            ),
            (
                "register chain",
                Accountant::Wtt,
                account(register_chain_layout(&registration())),
                Want::Kept(StateKind::Keyed(KeyedKind::RegisterChainRecords)),
            ),
            (
                "transceiver hub on ntt",
                Accountant::Ntt,
                hub(),
                Want::Kept(StateKind::Keyed(KeyedKind::TransceiverHubs)),
            ),
            (
                "transceiver peer on ntt",
                Accountant::Ntt,
                peer.clone(),
                Want::Kept(StateKind::Keyed(KeyedKind::TransceiverPeers)),
            ),
            (
                "pending observations",
                Accountant::Wtt,
                (pending.clone(), Pubkey::new_unique()),
                Want::Pending,
            ),
            (
                "empty at a keypair address",
                Accountant::Wtt,
                (vec![], keypair_address()),
                Want::Foreign,
            ),
            (
                "zero-filled at a keypair address",
                Accountant::Ntt,
                (vec![0; 70], keypair_address()),
                Want::Foreign,
            ),
            (
                "empty at a pda",
                Accountant::Wtt,
                (vec![], pda()),
                Want::Foreign,
            ),
            (
                "zero-filled at a pda",
                Accountant::Wtt,
                (vec![0; 70], pda()),
                Want::Foreign,
            ),
            (
                "zero-filled at an off-curve seed address",
                Accountant::Wtt,
                (vec![0; 70], off_curve_seed_address()),
                Want::Foreign,
            ),
            (
                "tag 0 with data at a pda",
                Accountant::Wtt,
                ([vec![0; 69], vec![1]].concat(), pda()),
                Want::Malformed("tag 0 with 70 bytes is not wtt state"),
            ),
            (
                "unknown tag at a pda",
                Accountant::Wtt,
                (vec![0xEE; 70], pda()),
                Want::Malformed("tag 238 with 70 bytes is not wtt state"),
            ),
            (
                "unknown tag at a keypair address",
                Accountant::Wtt,
                (vec![0xEE; 70], keypair_address()),
                Want::Malformed("tag 238 with 70 bytes is not wtt state"),
            ),
            (
                "wrong size",
                Accountant::Wtt,
                (
                    balance_bytes[..balance_bytes.len() - 1].to_vec(),
                    balance().1,
                ),
                Want::Malformed("bytes, want"),
            ),
            (
                "reencode rejects field",
                Accountant::Wtt,
                modify_bad_kind,
                Want::Malformed("with an invalid field"),
            ),
            (
                "non-canonical bytes",
                Accountant::Wtt,
                balance_padding,
                Want::Malformed("non-canonical bytes"),
            ),
            (
                "non-canonical address",
                Accountant::Wtt,
                (balance().0, Pubkey::new_unique()),
                Want::Malformed("non-canonical address"),
            ),
            (
                "transceiver hub on wtt",
                Accountant::Wtt,
                hub(),
                Want::Malformed("is not wtt state"),
            ),
            (
                "transceiver peer on wtt",
                Accountant::Wtt,
                peer,
                Want::Malformed("is not wtt state"),
            ),
            (
                "pending observations wrong size",
                Accountant::Wtt,
                (pending[..pending.len() - 1].to_vec(), Pubkey::new_unique()),
                Want::Malformed("PendingObservations tag with"),
            ),
        ];

        for (name, accountant, (data, address), want) in cases {
            let mut state = ChainState::default();
            let mut solana_only = SolanaOnlyFindings::default();
            classify_account(
                &mut state,
                &mut solana_only,
                &program(),
                accountant,
                &address,
                &data,
            )
            .unwrap_or_else(|e| panic!("{name}: {e:#}"));
            let kept: usize = KeyedKind::ALL
                .into_iter()
                .map(|kind| state.len_of(StateKind::Keyed(kind)))
                .sum();
            match want {
                Want::Kept(kind) => {
                    assert_eq!(state.len_of(kind), 1, "{name}: kept in {kind}");
                    assert_eq!(kept, 1, "{name}: kept once");
                    assert!(solana_only.malformed.is_empty(), "{name}: no finding");
                    assert_eq!(solana_only.pending_observations, 0, "{name}: not pending");
                    assert!(
                        solana_only.foreign_accounts.is_empty(),
                        "{name}: not foreign"
                    );
                }
                Want::Pending => {
                    assert_eq!(kept, 0, "{name}: not kept");
                    assert!(solana_only.malformed.is_empty(), "{name}: no finding");
                    assert_eq!(solana_only.pending_observations, 1, "{name}: pending");
                    assert!(
                        solana_only.foreign_accounts.is_empty(),
                        "{name}: not foreign"
                    );
                }
                Want::Foreign => {
                    assert_eq!(kept, 0, "{name}: not kept");
                    assert!(solana_only.malformed.is_empty(), "{name}: no finding");
                    assert_eq!(solana_only.pending_observations, 0, "{name}: not pending");
                    assert_eq!(
                        solana_only.foreign_accounts,
                        vec![address],
                        "{name}: foreign"
                    );
                }
                Want::Malformed(text) => {
                    assert_eq!(kept, 0, "{name}: not kept");
                    assert_eq!(solana_only.pending_observations, 0, "{name}: not pending");
                    assert!(
                        solana_only.foreign_accounts.is_empty(),
                        "{name}: not foreign"
                    );
                    assert_eq!(solana_only.malformed.len(), 1, "{name}: one finding");
                    let finding = &solana_only.malformed[0];
                    assert_eq!(finding.kind, FindingKind::MalformedAccounts, "{name}: kind");
                    assert_eq!(finding.key, address.to_string(), "{name}: key");
                    assert!(finding.expected.is_none(), "{name}: expected side");
                    let description = finding.actual.as_deref().unwrap();
                    assert!(
                        description.contains(text),
                        "{name}: {description:?} lacks {text:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn a_duplicate_account_is_fatal() {
        let (data, address) = balance();
        let mut state = ChainState::default();
        let mut solana_only = SolanaOnlyFindings::default();
        let mut classify = || {
            classify_account(
                &mut state,
                &mut solana_only,
                &program(),
                Accountant::Wtt,
                &address,
                &data,
            )
        };
        classify().unwrap();
        assert!(classify().is_err());
    }
}
