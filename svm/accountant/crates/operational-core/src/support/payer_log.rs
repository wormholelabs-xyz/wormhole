//! Payer-log emit. Layout: [`AccountantPayerLog`].

use crate::definitions::AccountantPayerLog;

/// Emit one payer log entry through `sol_log_data`.
pub fn emit(pending_pda: &[u8; 32], recorded_payer: &[u8; 32]) {
    let entry = AccountantPayerLog::new(*pending_pda, *recorded_payer);
    anchor_lang::solana_program::log::sol_log_data(&[entry.as_bytes()]);
}
