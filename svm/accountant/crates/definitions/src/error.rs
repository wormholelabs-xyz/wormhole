//! Stable custom error codes returned via `ProgramError::Custom(u32)`.

/// Custom error codes returned via `ProgramError::Custom(u32)`. Stable across
/// program versions; do not renumber.
#[repr(u32)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GlobalAccountantError {
    InvalidInstruction = 0,
    InvalidInstructionData = 1,
    InvalidPda = 2,
    DigestMismatch = 3,
    PayerMismatch = 4,
    /// Reserved slot; never raised. Kept to preserve ABI numbering.
    NotImplemented = 5,
    /// The instruction was feature-gated off in this build (keeps
    /// `test_only_open_digest` out of production).
    NotEnabled = 6,
    /// `(chain, emitter, sequence)` already marked accounted-for in NoReplay.
    AlreadyAccounted = 7,
    /// Retired — never raised. `pinocchio::cpi::invoke_signed` only surfaces
    /// pre-CPI validation errors via `Result`; an inner-program failure aborts
    /// THIS program directly via the SBF runtime, bypassing any `map_err`.
    /// Race-loss `AccountAlreadyInitialized` from `MarkUsed` propagates as
    /// itself, not as this code. Kept so error-code numbering stays stable.
    NoReplayCpiFailed = 8,
    /// Signature failed `secp256k1_recover`, or the recovered pubkey did not
    /// match the `guardian_index` in the GuardianSet PDA.
    InvalidSignature = 9,
    /// `guardian_index` out of bounds for the guardian set.
    InvalidGuardianIndex = 10,
    /// The guardian's bit is already set in the pending bitmap.
    AlreadySigned = 11,
    /// Observation references a guardian set older than the one the pending PDA
    /// is accumulating against (stale observation after rotation).
    StaleGuardianSet = 12,
    /// Retired — no longer emitted. Kept so error-code numbering stays stable.
    DigestForgery = 13,
    /// `close_pending` triggers unmet: recorded guardian set still active AND
    /// NoReplay does not mark the entry accounted-for.
    CannotCleanup = 14,
    /// Balance overflow on the transfer path (`lock_or_burn` /
    /// `unlock_or_mint`).
    BalanceOverflow = 15,
    /// Balance underflow on the transfer path (insufficient source balance).
    BalanceUnderflow = 16,
    // 17 reserved (previously `BodyDigestMismatch`; the digest is now derived
    // from the body in `submit_observations`, so a mismatch is unrepresentable).
    /// Supplied Account PDA does not match the canonical seeds for the
    /// source/destination side of the transfer.
    InvalidAccountPda = 18,
    /// No `ChainRegistration` PDA for the body's `emitter_chain` — wait for the
    /// Token Bridge `RegisterChain` VAA before observations are accepted.
    MissingChainRegistration = 19,
    /// Registration PDA exists but its `emitter_address` does not match the
    /// body header's emitter.
    UnregisteredEmitter = 20,
    /// `register_chain` body did not come from the governance emitter
    /// `(chain=1, GOVERNANCE_EMITTER)`.
    InvalidGovernanceEmitter = 21,
    /// `register_chain` payload module is not `TOKEN_BRIDGE_GOVERNANCE_MODULE`.
    InvalidGovernanceModule = 22,
    /// `register_chain` payload action byte is not `0x01` (RegisterChain).
    InvalidGovernanceAction = 23,
    /// `register_chain` target chain is neither `0x0000` (Any) nor Solana.
    GovernanceChainMismatch = 24,
    /// `modify_balance` `kind` byte is neither `1` (Add) nor `2` (Subtract).
    InvalidModificationKind = 25,
    /// `modify_balance` `Add` overflow. Distinct from `BalanceOverflow` so logs
    /// disambiguate the entrypoint.
    ModifyBalanceOverflow = 26,
    /// `modify_balance` `Subtract` underflow (also raised when subtracting from
    /// an uninitialised PDA, rejected before allocation).
    ModifyBalanceUnderflow = 27,
    /// A `Modification` PDA already exists at `(b"modification", sequence)`.
    /// Replay protection keyed on the payload's modification sequence.
    DuplicateModification = 28,
    /// Token Bridge payload action is not `0x01`/`0x02`/`0x03`. Rejecting
    /// (rather than committing) leaves the NoReplay slot unconsumed so a future
    /// upgrade can process the VAA.
    UnknownTokenBridgePayload = 29,
    /// NTT: no `TransceiverHub` PDA registered for the routing key
    /// `(emitter_chain, sender)`. The transfer cannot be assigned a hub token
    /// identity, so it is rejected (NoReplay slot left unconsumed).
    MissingTransceiverHub = 30,
    /// NTT: a required `TransceiverPeer` PDA is missing for the peer
    /// cross-registration check (either direction).
    MissingTransceiverPeer = 31,
    /// NTT: the bidirectional peer cross-registration does not agree — the
    /// destination's registered peer for the source chain is not the source
    /// transceiver. The transfer is rejected.
    PeerRegistrationMismatch = 32,
    /// NTT `register_hub`: the transceiver-info message is Burning-mode. Only
    /// Locking-mode info registers a hub; CosmWasm bails with "ignoring
    /// non-locking NTT initialization". Rejected, NoReplay slot left unconsumed.
    NotLockingHub = 33,
    /// NTT `register_hub`: a `TransceiverHub` PDA already exists at
    /// `(emitter_chain, emitter_address)`. CosmWasm bails with "hub entry already
    /// exists" — re-registering a hub is not allowed.
    DuplicateTransceiverHub = 34,
    /// NTT `register_peer`: a `TransceiverPeer` PDA already exists at
    /// `(emitter_chain, emitter_address, dest_chain)`. CosmWasm bails with "peer
    /// entry for this chain already exists".
    DuplicateTransceiverPeer = 35,
    /// NTT `register_peer`: the source transceiver has no known hub and the peer
    /// it is registering is not itself a hub. CosmWasm bails with "ignoring
    /// attempt to register peer before hub" — the hub must be registered first.
    PeerBeforeHub = 36,
}

impl From<GlobalAccountantError> for u32 {
    fn from(e: GlobalAccountantError) -> Self {
        e as u32
    }
}
