//! Solana side of the comparison.
//!
//! Program accounts come from one unfiltered `getProgramAccounts`, so a
//! sample is one view and accounts of every tag are seen. Each account is
//! parsed through its definitions layout, re-encoded with `Layout::new`, and
//! must equal the original bytes and sit at the PDA its own key derives.
//! Anything else is an [`ComparedState::MalformedAccounts`] finding.
//!
//! NoReplay bucket PDAs are one-way, so they cannot be listed; coverage is the
//! `(chain, emitter, bucket)` universe of the expected replay set, fetched with
//! batched `getMultipleAccounts`. Every set bit of a fetched bucket is read
//! back, so an extra mark inside that universe is a finding.

use anyhow::{bail, Context, Result};
use ga_backfill::accountant::Accountant;
use ga_backfill::noreplay::{derive_noreplay_authority_pda, fetch_buckets, BucketAccount};
use global_accountant_definitions::{
    AccountLayout, AccountTag, BalanceAccountLayout, ChainRegistrationLayout, ModifyBalanceLayout,
    PendingObservationsLayout, RegisterChainLayout, TransceiverHubLayout, TransceiverPeerLayout,
};
use solana_client::nonblocking::rpc_client::RpcClient;
use solana_client::rpc_config::{
    RpcAccountInfoConfig, RpcProgramAccountsConfig, UiAccountEncoding,
};
use solana_pubkey::Pubkey;

use crate::compare::Finding;
use crate::compared_account::{account_tag, ComparedAccount};
use crate::state::{BucketId, ChainState, ComparedState, ReplayMarks};

/// Program accounts per sample. Mainnet: 17,367 balances plus tens of others,
/// plus live `PendingObservations` after the cutover.
pub const MAX_PROGRAM_ACCOUNTS: usize = 2_000_000;

/// Solana-only findings: malformed program accounts and the pending count.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ScanFindings {
    pub malformed: Vec<Finding>,
    pub pending: usize,
}

/// One sample of the program's state.
pub async fn fetch_onchain_state(
    rpc: &RpcClient,
    program_id: &Pubkey,
    noreplay_program: &Pubkey,
    accountant: Accountant,
    replay_universe: &ReplayMarks,
) -> Result<(ChainState, ScanFindings)> {
    let mut state = ChainState::default();
    let mut scan = ScanFindings::default();

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
        let finding = sort_account(
            &mut state,
            &mut scan,
            program_id,
            accountant,
            address,
            &account.data,
        )?;
        if let Some(finding) = finding {
            scan.malformed.push(malformed_account(address, finding));
        }
    }

    fetch_replay(
        rpc,
        program_id,
        noreplay_program,
        replay_universe,
        &mut state,
        &mut scan,
    )
    .await?;
    Ok((state, scan))
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

fn malformed_account(address: &Pubkey, finding: String) -> Finding {
    Finding {
        kind: ComparedState::MalformedAccounts,
        key: address.to_string(),
        wormchain: None,
        solana: Some(finding),
    }
}

/// Sort one program account into its compared state. `Ok(Some(_))` describes an
/// account that fits no layout; `Err` is a fatal state error (duplicate, cap).
fn sort_account(
    state: &mut ChainState,
    scan: &mut ScanFindings,
    program_id: &Pubkey,
    accountant: Accountant,
    address: &Pubkey,
    data: &[u8],
) -> Result<Option<String>> {
    let Some(&tag_byte) = data.first() else {
        return Ok(Some("empty account".to_string()));
    };
    let insert_as = match account_tag(tag_byte) {
        Some(AccountTag::PendingObservations) => {
            if data.len() != PendingObservationsLayout::LEN {
                return Ok(Some(format!(
                    "PendingObservations tag with {} bytes",
                    data.len()
                )));
            }
            scan.pending += 1;
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
/// address, and add it to `L`'s compared state.
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
async fn fetch_replay(
    rpc: &RpcClient,
    program_id: &Pubkey,
    noreplay_program: &Pubkey,
    universe: &ReplayMarks,
    state: &mut ChainState,
    scan: &mut ScanFindings,
) -> Result<()> {
    let authority = derive_noreplay_authority_pda(program_id);
    let buckets: Vec<BucketId> = universe.buckets().keys().copied().collect();
    let fetched = fetch_buckets(rpc, noreplay_program, &authority, &buckets).await?;
    for (bucket, (pda, state_of)) in buckets.into_iter().zip(fetched) {
        let finding = match state_of {
            BucketAccount::Missing => continue,
            BucketAccount::Bitmap(bitmap) => {
                state.replay.insert_bucket(bucket, bitmap.bitmap)?;
                continue;
            }
            BucketAccount::Foreign(owner) => format!("bucket owned by {owner}"),
            BucketAccount::Malformed(len) => format!("bucket with {len} bytes"),
        };
        scan.malformed.push(malformed_account(&pda, finding));
    }
    Ok(())
}
