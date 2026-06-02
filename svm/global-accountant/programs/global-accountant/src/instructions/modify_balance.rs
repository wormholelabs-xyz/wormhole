//! `modify_balance` — Accountant governance handler.
//!
//! Port of CosmWasm `handle_accountant_governance_vaa`
//! (`cosmwasm/contracts/global-accountant/src/contract.rs:399-440`) plus
//! the `modify_balance` helper at
//! `cosmwasm/packages/accountant/src/contract.rs:244-278`. Applies a
//! manual Add / Subtract delta to the canonical `BalanceAccount` PDA via a
//! Wormchain-emitted governance VAA. Used for post-incident ledger
//! reconciliation when an off-chain event (exploit, manual mint, chain
//! rollback) requires the on-chain balance to be corrected.
//!
//! Replay protection keys on the payload `sequence`: the `ModificationLog`
//! PDA is per-sequence (not per-balance), so two distinct governance VAAs
//! targeting the same `(chain, token_chain, token_address)` triple cannot
//! collide. CosmWasm rejects `Any (0)` as `target_chain` on this path —
//! only `WORMCHAIN_CHAIN_ID` (`contract.rs:404-407`).

use pinocchio::{
    cpi::{Seed, Signer},
    error::ProgramError,
    instruction::{InstructionAccount, InstructionView},
    AccountView, Address, ProgramResult,
};

use crate::definitions::{
    BalanceAccountLayout, GlobalAccountantError, ModificationKind, ModificationLogLayout, Uint256,
    ACCOUNTANT_GOVERNANCE_MODULE, ACCOUNT_SEED_PREFIX, GOVERNANCE_EMITTER,
    MODIFICATION_SEED_PREFIX, MODIFY_BALANCE_ACTION, SOLANA_CHAIN_ID, VERIFY_HASH_DATA_LEN,
    VERIFY_HASH_SELECTOR, WORMCHAIN_CHAIN_ID,
};
use crate::err;
use crate::instructions::pda_init::init_or_upgrade_pda;
use crate::state::{account as balance_account, modification};

// ============================================================================
// Wire format
// ============================================================================

/// Wire format for the `modify_balance` instruction data (after the 1-byte
/// dispatch discriminator):
///
/// | offset | size     | field             |
/// |--------|----------|-------------------|
/// | 0      | 1        | guardian_set_bump |
/// | 1      | 1        | balance_pda_bump  |
/// | 2      | 1        | modification_bump |
/// | 3      | 2        | body_len (LE)     |
/// | 5      | body_len | body              |
const MODIFY_BALANCE_FIXED_LEN: usize = 1 + 1 + 1 + 2;

/// Maximum VAA body size accepted. The canonical ModifyBalance body is 195
/// bytes (51-byte header + 32-byte module + 1-byte action + 2-byte target
/// chain + 109-byte payload); 256 leaves headroom for any future field while
/// staying well inside Solana's 1232-byte tx envelope.
const MODIFY_BALANCE_BODY_MAX: usize = 256;

/// Body header offsets (canonical Wormhole VAA layout, 51-byte header).
const BODY_EMITTER_CHAIN_OFFSET: usize = 8;
const BODY_EMITTER_ADDRESS_OFFSET: usize = 10;
const BODY_HEADER_LEN: usize = 51;

/// Payload byte offsets (relative to body start). Layout:
///
/// | offset                | size | field          |
/// |-----------------------|------|----------------|
/// | BODY_HEADER_LEN       | 32   | module         |
/// | +32                   | 1    | action         |
/// | +33                   | 2    | target_chain   |
/// | +35                   | 8    | payload_seq    |
/// | +43                   | 2    | chain_id       |
/// | +45                   | 2    | token_chain    |
/// | +47                   | 32   | token_address  |
/// | +79                   | 1    | kind           |
/// | +80                   | 32   | amount         |
/// | +112                  | 32   | reason         |
const PAYLOAD_MODULE_OFFSET: usize = BODY_HEADER_LEN;
const PAYLOAD_ACTION_OFFSET: usize = BODY_HEADER_LEN + 32;
const PAYLOAD_TARGET_CHAIN_OFFSET: usize = BODY_HEADER_LEN + 33;
const PAYLOAD_SEQUENCE_OFFSET: usize = BODY_HEADER_LEN + 35;
const PAYLOAD_CHAIN_ID_OFFSET: usize = BODY_HEADER_LEN + 43;
const PAYLOAD_TOKEN_CHAIN_OFFSET: usize = BODY_HEADER_LEN + 45;
const PAYLOAD_TOKEN_ADDRESS_OFFSET: usize = BODY_HEADER_LEN + 47;
const PAYLOAD_KIND_OFFSET: usize = BODY_HEADER_LEN + 79;
const PAYLOAD_AMOUNT_OFFSET: usize = BODY_HEADER_LEN + 80;
const PAYLOAD_REASON_OFFSET: usize = BODY_HEADER_LEN + 112;
const PAYLOAD_TOTAL_LEN: usize = 32 + 1 + 2 + 8 + 2 + 2 + 32 + 1 + 32 + 32;
const BODY_MIN_LEN: usize = BODY_HEADER_LEN + PAYLOAD_TOTAL_LEN;

pub fn process(program_id: &Address, accounts: &mut [AccountView], data: &[u8]) -> ProgramResult {
    // ----- (1) Parse wire data -----
    if data.len() < MODIFY_BALANCE_FIXED_LEN {
        return Err(err(GlobalAccountantError::InvalidInstructionData));
    }
    let guardian_set_bump = data[0];
    let balance_pda_bump = data[1];
    let modification_bump = data[2];
    let body_len = u16::from_le_bytes([data[3], data[4]]) as usize;
    if !(BODY_MIN_LEN..=MODIFY_BALANCE_BODY_MAX).contains(&body_len)
        || data.len() != MODIFY_BALANCE_FIXED_LEN + body_len
    {
        return Err(err(GlobalAccountantError::InvalidInstructionData));
    }
    let body_bytes = &data[MODIFY_BALANCE_FIXED_LEN..MODIFY_BALANCE_FIXED_LEN + body_len];

    // ----- (2) Compute digest -----
    let digest = double_keccak256(body_bytes);

    // Accounts:
    //   0. `[WRITE, SIGNER]` payer.
    //   1. `[]`              Verify VAA Shim program (CPI target).
    //   2. `[]`              Core Bridge `GuardianSet` PDA.
    //   3. `[]`              `GuardianSignatures` PDA.
    //   4. `[WRITE]`         `BalanceAccount` PDA — lazy-init on first Add for
    //                       a fresh (chain, token_chain, token_address);
    //                       required to exist for Sub.
    //   5. `[]`              system program.
    //   6. `[WRITE]`         `ModificationLog` PDA at
    //                       `(b"modification", payload_sequence_be)`.
    //                       Lazy-inited every call; existence ⇒
    //                       `DuplicateModification`.
    let [payer, verify_vaa_shim_program, guardian_set, guardian_signatures, balance_pda, _system_program_acc, modification_pda] =
        accounts
    else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    if !payer.is_signer() {
        return Err(ProgramError::MissingRequiredSignature);
    }

    // ----- (3) Shim CPI to verify the digest -----
    verify_vaa(
        verify_vaa_shim_program,
        guardian_set,
        guardian_signatures,
        &digest,
        guardian_set_bump,
    )?;

    // ----- (4) Governance emitter check -----
    let body_emitter_chain = u16::from_be_bytes([
        body_bytes[BODY_EMITTER_CHAIN_OFFSET],
        body_bytes[BODY_EMITTER_CHAIN_OFFSET + 1],
    ]);
    if body_emitter_chain != SOLANA_CHAIN_ID
        || body_bytes[BODY_EMITTER_ADDRESS_OFFSET..BODY_EMITTER_ADDRESS_OFFSET + 32]
            != GOVERNANCE_EMITTER
    {
        return Err(err(GlobalAccountantError::InvalidGovernanceEmitter));
    }

    // ----- (5) Payload validation -----
    if body_bytes[PAYLOAD_MODULE_OFFSET..PAYLOAD_MODULE_OFFSET + 32] != ACCOUNTANT_GOVERNANCE_MODULE
    {
        return Err(err(GlobalAccountantError::InvalidGovernanceModule));
    }
    if body_bytes[PAYLOAD_ACTION_OFFSET] != MODIFY_BALANCE_ACTION {
        return Err(err(GlobalAccountantError::InvalidGovernanceAction));
    }
    let target_chain = u16::from_be_bytes([
        body_bytes[PAYLOAD_TARGET_CHAIN_OFFSET],
        body_bytes[PAYLOAD_TARGET_CHAIN_OFFSET + 1],
    ]);
    // CosmWasm `handle_accountant_governance_vaa` rejects target chains other
    // than `Wormchain` (no `Any` acceptance, unlike Token Bridge governance).
    if target_chain != WORMCHAIN_CHAIN_ID {
        return Err(err(GlobalAccountantError::GovernanceChainMismatch));
    }
    let kind_byte = body_bytes[PAYLOAD_KIND_OFFSET];
    let kind = ModificationKind::from_u8(kind_byte)
        .ok_or_else(|| err(GlobalAccountantError::InvalidModificationKind))?;

    // ----- (6) Parse modification fields -----
    let payload_sequence = u64::from_be_bytes(
        body_bytes[PAYLOAD_SEQUENCE_OFFSET..PAYLOAD_SEQUENCE_OFFSET + 8]
            .try_into()
            .map_err(|_| err(GlobalAccountantError::InvalidInstructionData))?,
    );
    let chain_id = u16::from_be_bytes([
        body_bytes[PAYLOAD_CHAIN_ID_OFFSET],
        body_bytes[PAYLOAD_CHAIN_ID_OFFSET + 1],
    ]);
    let token_chain = u16::from_be_bytes([
        body_bytes[PAYLOAD_TOKEN_CHAIN_OFFSET],
        body_bytes[PAYLOAD_TOKEN_CHAIN_OFFSET + 1],
    ]);
    let mut token_address = [0u8; 32];
    token_address.copy_from_slice(
        &body_bytes[PAYLOAD_TOKEN_ADDRESS_OFFSET..PAYLOAD_TOKEN_ADDRESS_OFFSET + 32],
    );
    let mut amount_bytes = [0u8; 32];
    amount_bytes.copy_from_slice(&body_bytes[PAYLOAD_AMOUNT_OFFSET..PAYLOAD_AMOUNT_OFFSET + 32]);
    let amount = Uint256(amount_bytes);
    let mut reason = [0u8; 32];
    reason.copy_from_slice(&body_bytes[PAYLOAD_REASON_OFFSET..PAYLOAD_REASON_OFFSET + 32]);

    // ----- (7) Canonical-PDA enforcement -----
    let chain_id_be = chain_id.to_be_bytes();
    let token_chain_be = token_chain.to_be_bytes();
    let (expected_balance_pda, canonical_balance_bump) = Address::find_program_address(
        &[
            ACCOUNT_SEED_PREFIX,
            &chain_id_be,
            &token_chain_be,
            &token_address,
        ],
        program_id,
    );
    if balance_pda.address() != &expected_balance_pda || balance_pda_bump != canonical_balance_bump
    {
        return Err(err(GlobalAccountantError::InvalidPda));
    }

    let payload_sequence_be = payload_sequence.to_be_bytes();
    let (expected_modification_pda, canonical_modification_bump) = Address::find_program_address(
        &[MODIFICATION_SEED_PREFIX, &payload_sequence_be],
        program_id,
    );
    if modification_pda.address() != &expected_modification_pda
        || modification_bump != canonical_modification_bump
    {
        return Err(err(GlobalAccountantError::InvalidPda));
    }

    // ----- (8) Replay protection -----
    //
    // `ModificationLog` PDA must be system-owned (uninitialised). Existence
    // signals a prior `modify_balance` call already consumed this payload
    // sequence — surface `DuplicateModification`. Mirrors CosmWasm
    // `MODIFICATIONS.has(deps.storage, msg.sequence)` early-bail check at
    // `packages/accountant/src/contract.rs:248-250`.
    if modification_pda.owner() != &pinocchio_system::ID {
        return Err(err(GlobalAccountantError::DuplicateModification));
    }

    // ----- (9) Apply the delta -----
    //
    // Sub on uninit must reject BEFORE allocation so the payer doesn't pay
    // rent on a guaranteed-failed mutation. Add on uninit lazy-inits + writes
    // a fresh layout with `balance = amount`. Existing PDAs go through
    // `raw_add` / `raw_sub` and persist back via `balance_account::store`.
    let balance_is_uninit = balance_pda.owner() == &pinocchio_system::ID;
    if balance_is_uninit {
        match kind {
            ModificationKind::Subtract => {
                // 0 - amount underflows for any amount > 0. amount == 0 is a
                // no-op but still pointless on uninit; reject either way.
                return Err(err(GlobalAccountantError::ModifyBalanceUnderflow));
            }
            ModificationKind::Add => {
                init_balance_account(
                    program_id,
                    payer,
                    balance_pda,
                    canonical_balance_bump,
                    chain_id,
                    token_chain,
                    &token_address,
                    amount,
                )?;
            }
        }
    } else {
        // Existing balance PDA. Owner check guards against foreign-program
        // accounts at the canonical address (the runtime forbids assignment to
        // our seed by another program, but the defensive check costs ~50 CU
        // and makes the intent explicit).
        if balance_pda.owner() != program_id {
            return Err(err(GlobalAccountantError::InvalidPda));
        }
        let mut layout = balance_account::load(balance_pda)?;
        match kind {
            ModificationKind::Add => layout.raw_add(amount).map_err(err)?,
            ModificationKind::Subtract => layout.raw_sub(amount).map_err(err)?,
        }
        balance_account::store(balance_pda, &layout)?;
    }

    // ----- (10) Lazy-init the ModificationLog PDA + store -----
    let bump_seed = [canonical_modification_bump];
    let seeds = [
        Seed::from(MODIFICATION_SEED_PREFIX),
        Seed::from(payload_sequence_be.as_slice()),
        Seed::from(bump_seed.as_slice()),
    ];
    let signer = Signer::from(&seeds);
    init_or_upgrade_pda(
        payer,
        modification_pda,
        program_id,
        signer,
        ModificationLogLayout::LEN as u64,
    )?;

    let mut log: ModificationLogLayout = bytemuck::Zeroable::zeroed();
    log.sequence = payload_sequence;
    log.chain_id = chain_id;
    log.token_chain = token_chain;
    log.kind = kind_byte;
    log.token_address = token_address;
    log.amount = amount;
    log.reason = reason;
    modification::store(modification_pda, &log)?;

    // ----- (11) Log the modification for off-chain consumers -----
    //
    // Solana's `sol_log` ABI is the cheapest way to surface the reason field
    // without paying for additional on-chain storage. Off-chain indexers
    // already scrape program logs; this gives them parity with CosmWasm's
    // event-attribute audit channel. Logging the reason via the unsafe
    // syscall keeps the no_std target build clean (no formatting infra).
    log_modification(payload_sequence, chain_id, kind_byte, &reason);

    Ok(())
}

/// Lazy-init the `BalanceAccount` PDA with the `Add`-on-uninit shape:
/// allocate via `init_or_upgrade_pda`, then stamp a freshly-zeroed layout
/// with `balance = amount`.
#[allow(clippy::too_many_arguments)]
fn init_balance_account(
    program_id: &Address,
    payer: &AccountView,
    balance_pda: &mut AccountView,
    canonical_bump: u8,
    chain_id: u16,
    token_chain: u16,
    token_address: &[u8; 32],
    amount: Uint256,
) -> ProgramResult {
    let chain_id_be = chain_id.to_be_bytes();
    let token_chain_be = token_chain.to_be_bytes();
    let bump_seed = [canonical_bump];
    let seeds = [
        Seed::from(ACCOUNT_SEED_PREFIX),
        Seed::from(chain_id_be.as_slice()),
        Seed::from(token_chain_be.as_slice()),
        Seed::from(token_address.as_slice()),
        Seed::from(bump_seed.as_slice()),
    ];
    let signer = Signer::from(&seeds);

    init_or_upgrade_pda(
        payer,
        balance_pda,
        program_id,
        signer,
        BalanceAccountLayout::LEN as u64,
    )?;

    let mut layout: BalanceAccountLayout = bytemuck::Zeroable::zeroed();
    layout.chain = chain_id;
    layout.token_chain = token_chain;
    layout.token_address = *token_address;
    layout.balance = amount;
    balance_account::store(balance_pda, &layout)
}

// ============================================================================
// Verify VAA Shim CPI — shape mirrored from `register_chain::verify_vaa`.
// ============================================================================

fn verify_vaa(
    verify_vaa_shim_program: &AccountView,
    guardian_set: &AccountView,
    guardian_signatures: &AccountView,
    digest: &[u8; 32],
    guardian_set_bump: u8,
) -> ProgramResult {
    if verify_vaa_shim_program.address().as_array()
        != &crate::definitions::VERIFY_VAA_SHIM_PROGRAM_ID
    {
        return Err(err(GlobalAccountantError::InvalidPda));
    }

    let mut ix_data = [0u8; VERIFY_HASH_DATA_LEN];
    ix_data[..8].copy_from_slice(&VERIFY_HASH_SELECTOR);
    ix_data[8] = guardian_set_bump;
    ix_data[9..].copy_from_slice(digest);

    let ix_accounts = [
        InstructionAccount::readonly(guardian_set.address()),
        InstructionAccount::readonly(guardian_signatures.address()),
    ];

    let instruction = InstructionView {
        program_id: verify_vaa_shim_program.address(),
        data: &ix_data,
        accounts: &ix_accounts,
    };

    pinocchio::cpi::invoke(&instruction, &[guardian_set, guardian_signatures])
}

use crate::hash::double_keccak256;

// ============================================================================
// Logging — emits the modification record to the SBF program log so
// off-chain indexers can replay the audit trail without walking the VAA
// archive. No-op on host builds.
// ============================================================================

#[cfg(any(target_os = "solana", target_arch = "bpf"))]
fn log_modification(sequence: u64, chain_id: u16, kind: u8, reason: &[u8; 32]) {
    // sol_log_64_ captures the structured fields; sol_log_ on the reason
    // bytes captures the audit string. Both are constant-cost syscalls.
    // SAFETY: pinocchio re-exports the canonical Solana syscall ABIs;
    // sol_log_64_ takes five u64 params, sol_log_ takes (ptr, len).
    unsafe {
        pinocchio::syscalls::sol_log_64_(sequence, chain_id as u64, kind as u64, 0, 0);
        pinocchio::syscalls::sol_log_(reason.as_ptr(), reason.len() as u64);
    }
}

#[cfg(not(any(target_os = "solana", target_arch = "bpf")))]
fn log_modification(_sequence: u64, _chain_id: u16, _kind: u8, _reason: &[u8; 32]) {}
