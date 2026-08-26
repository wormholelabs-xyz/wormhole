//! Custom error codes returned as `ProgramError::Custom(u32)`.

/// Custom error codes. Do not renumber.
#[repr(u32)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GlobalAccountantError {
    InvalidInstruction = 0,
    InvalidInstructionData = 1,
    InvalidPda = 2,
    DigestMismatch = 3,
    PayerMismatch = 4,
    /// Reserved; keeps numbering stable.
    NotImplemented = 5,
    /// Instruction is feature-gated off in this build.
    NotEnabled = 6,
    /// `(chain, emitter, sequence)` already marked in NoReplay.
    AlreadyAccounted = 7,
    /// Reserved; keeps numbering stable.
    NoReplayCpiFailed = 8,
    /// `secp256k1_recover` failed, or the recovered key differs from `guardian_index`.
    InvalidSignature = 9,
    /// `guardian_index` out of bounds for the guardian set.
    InvalidGuardianIndex = 10,
    /// Guardian bit already set in the pending bitmap.
    AlreadySigned = 11,
    /// Observation guardian set is older than the pending PDA's set.
    StaleGuardianSet = 12,
    /// Reserved; keeps numbering stable.
    DigestForgery = 13,
    /// `close_pending` conditions unmet: guardian set still active and NoReplay unmarked.
    CannotCleanup = 14,
    /// Balance overflow in `lock_or_burn` / `unlock_or_mint`.
    BalanceOverflow = 15,
    /// Balance underflow in `lock_or_burn` / `unlock_or_mint`.
    BalanceUnderflow = 16,
    // 17 reserved.
    /// Balance PDA differs from the canonical seeds for the transfer side.
    InvalidAccountPda = 18,
    /// No `ChainRegistration` PDA for the body's `emitter_chain`.
    MissingChainRegistration = 19,
    /// `ChainRegistration.emitter_address` differs from the body emitter.
    UnregisteredEmitter = 20,
    /// `register_chain` emitter is not `(chain=1, GOVERNANCE_EMITTER)`.
    InvalidGovernanceEmitter = 21,
    /// `register_chain` module is not `TOKEN_BRIDGE_GOVERNANCE_MODULE`.
    InvalidGovernanceModule = 22,
    /// `register_chain` action byte is not `0x01`.
    InvalidGovernanceAction = 23,
    /// `register_chain` target chain is neither `0x0000` nor Solana.
    GovernanceChainMismatch = 24,
    /// `modify_balance` `kind` byte is neither `1` nor `2`.
    InvalidModificationKind = 25,
    /// `modify_balance` `Add` overflow.
    ModifyBalanceOverflow = 26,
    /// `modify_balance` `Subtract` underflow, including on an uninitialised PDA.
    ModifyBalanceUnderflow = 27,
    /// `Modification` PDA already exists for this sequence.
    DuplicateModification = 28,
    /// Token Bridge action byte is not `0x01`, `0x02`, or `0x03`. The NoReplay slot stays free.
    UnknownTokenBridgePayload = 29,
}

impl From<GlobalAccountantError> for u32 {
    fn from(e: GlobalAccountantError) -> Self {
        e as u32
    }
}
