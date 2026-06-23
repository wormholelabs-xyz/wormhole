//! Wormhole Global Accountant — Solana port (Pinocchio).
//!
//! The product-neutral operational machinery (quorum tracker, signed-VAA
//! backfill, pending cleanup, NoReplay/Shim CPIs, PDA-init, commit-log, hashing,
//! and the zero-copy state layouts) lives in `accountant-operational-core`. This
//! crate keeps the WTT-specific governance handlers (`register_chain`,
//! `modify_balance`), the Token Bridge transfer applicator (`transfer`), and the
//! entrypoint that wires the core handlers to a Token Bridge balance-application
//! callback.

// `no_std` on the SBF target (and `bpf`, so nightly clippy can lint the
// SBF-shaped code — platform-tools ships no clippy).
#![cfg_attr(any(target_os = "solana", target_arch = "bpf"), no_std)]
// `target_os = "solana"` is unknown to the host toolchain.
#![allow(unexpected_cfgs)]

pub mod entrypoint;
pub mod instructions;
pub mod state;

pub use global_accountant_definitions as definitions;

// Re-export so the staying handlers' `crate::err(...)` calls resolve to the
// single definition in `accountant-operational-core`.
pub use accountant_operational_core::err;
