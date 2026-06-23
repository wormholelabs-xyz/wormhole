//! Wormhole Global Accountant Backfill — Solana port (Pinocchio).

#![cfg_attr(any(target_os = "solana", target_arch = "bpf"), no_std)]
#![allow(unexpected_cfgs)]

pub mod entrypoint;
pub mod instructions;

pub use accountant_backfill_core::{definitions, err};
pub use global_accountant_definitions;
