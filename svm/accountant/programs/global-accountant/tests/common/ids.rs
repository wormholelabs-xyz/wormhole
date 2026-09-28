use solana_pubkey::Pubkey;

/// The accountant's `declare_id!` address. Both suites load the program here.
pub fn program_id() -> Pubkey {
    Pubkey::new_from_array(global_accountant::ID.to_bytes())
}
