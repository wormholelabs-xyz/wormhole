//! Backfill target: the accountant program a run writes to, its operator
//! authority, and its instruction discriminators.

use anyhow::{anyhow, ensure, Result};
use solana_pubkey::Pubkey;

use global_accountant_backfill::{Instruction as WttIx, BACKFILL_AUTHORITY};
use global_accountant_definitions::NOREPLAY_PROGRAM_ID;
use ntt_global_accountant_backfill::{Instruction as NttIx, NTT_BACKFILL_AUTHORITY};
use svm_accountant_tools_common::accountant::Accountant;
use svm_accountant_tools_common::noreplay::derive_noreplay_authority_pda;

/// One discriminator per backfill instruction. The NTT program numbers its shared
/// instructions like the WTT program; [`AccountantProgram::discriminators`] reads both enums.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Discriminators {
    pub noreplay: u8,
    pub balance: u8,
    pub modify_balance: u8,
    pub chain_registration: u8,
    /// `Some` on NTT only: the transceiver maps are NTT state.
    pub transceiver_hub: Option<u8>,
    pub transceiver_peer: Option<u8>,
}

/// Facts of the backfill program crates, which only the write side links.
pub trait AccountantProgram {
    /// Signer the backfill `.so` requires on every instruction.
    fn authority(self) -> Pubkey;
    fn discriminators(self) -> Discriminators;
}

impl AccountantProgram for Accountant {
    fn authority(self) -> Pubkey {
        match self {
            Self::Wtt => Pubkey::new_from_array(BACKFILL_AUTHORITY),
            Self::Ntt => Pubkey::new_from_array(NTT_BACKFILL_AUTHORITY),
        }
    }

    fn discriminators(self) -> Discriminators {
        match self {
            Self::Wtt => Discriminators {
                noreplay: WttIx::BackfillNoReplay as u8,
                balance: WttIx::BackfillBalance as u8,
                modify_balance: WttIx::BackfillModifyBalance as u8,
                chain_registration: WttIx::BackfillChainRegistration as u8,
                transceiver_hub: None,
                transceiver_peer: None,
            },
            Self::Ntt => Discriminators {
                noreplay: NttIx::BackfillNoReplay as u8,
                balance: NttIx::BackfillBalance as u8,
                modify_balance: NttIx::BackfillModifyBalance as u8,
                chain_registration: NttIx::BackfillRelayerChainRegistration as u8,
                transceiver_hub: Some(NttIx::BackfillTransceiverHub as u8),
                transceiver_peer: Some(NttIx::BackfillTransceiverPeer as u8),
            },
        }
    }
}

/// Common context for every backfill ix.
///
/// **Invariant**: `program_id` equals the accountant's `declare_id!` address and
/// `payer` equals the accountant's backfill authority. [`BackfillTarget::try_new`]
/// checks both; the program rejects any other value.
#[derive(Debug, Clone)]
pub struct BackfillTarget {
    accountant: Accountant,
    program_id: Pubkey,
    payer: Pubkey,
    noreplay_program: Pubkey,
    noreplay_authority: Pubkey,
    discriminators: Discriminators,
}

impl BackfillTarget {
    /// Fails on a program id or payer the `.so` would reject: every transaction
    /// would fail on-chain, so the misconfiguration states before any fee is paid.
    pub fn try_new(accountant: Accountant, program_id: Pubkey, payer: Pubkey) -> Result<Self> {
        ensure!(
            program_id == accountant.program_id(),
            "--program-id {program_id} is not the {accountant} backfill `declare_id!` {}",
            accountant.program_id()
        );
        ensure!(
            payer == accountant.authority(),
            "payer {payer} is not the {accountant} backfill authority {}; the program rejects every tx",
            accountant.authority()
        );
        Ok(Self {
            accountant,
            program_id,
            payer,
            noreplay_program: Pubkey::new_from_array(NOREPLAY_PROGRAM_ID),
            noreplay_authority: derive_noreplay_authority_pda(&program_id),
            discriminators: accountant.discriminators(),
        })
    }

    pub fn accountant(&self) -> Accountant {
        self.accountant
    }

    pub fn program_id(&self) -> Pubkey {
        self.program_id
    }

    pub fn payer(&self) -> Pubkey {
        self.payer
    }

    pub fn system_program(&self) -> Pubkey {
        Pubkey::new_from_array([0u8; 32])
    }

    pub fn noreplay_program(&self) -> Pubkey {
        self.noreplay_program
    }

    /// `noreplay_authority` PDA of `program_id`.
    pub fn noreplay_authority(&self) -> Pubkey {
        self.noreplay_authority
    }

    pub fn discriminators(&self) -> &Discriminators {
        &self.discriminators
    }

    /// `Some` on NTT only: the transceiver maps are NTT state.
    pub(crate) fn transceiver_discriminator(&self, disc: Option<u8>, kind: &str) -> Result<u8> {
        disc.ok_or_else(|| anyhow!("{kind} rows on a {} accountant", self.accountant))
    }
}
