//! Wormhole Global Accountant Backfill — Solana port (Pinocchio).

#![cfg_attr(any(target_os = "solana", target_arch = "bpf"), no_std)]
#![allow(unexpected_cfgs)]

pub mod entrypoint;
pub mod instructions;

pub use accountant_backfill_core::{definitions, err, BackfillError, BACKFILL_AUTHORITY};
pub use global_accountant_definitions;

/// Instruction discriminators. Single-byte prefix on instruction data.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Instruction {
    BackfillNoReplay = 0,
    BackfillBalance = 1,
}

impl Instruction {
    pub const fn from_u8(value: u8) -> Option<Self> {
        match value {
            0 => Some(Self::BackfillNoReplay),
            1 => Some(Self::BackfillBalance),
            _ => None,
        }
    }
}
