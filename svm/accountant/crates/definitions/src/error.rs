//! Custom error codes returned as `ProgramError::Custom(u32)` by both accountant programs
//! (the Global Accountant family: WTT and NTT). Variants are shared unless marked.

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
    /// Observation guardian set is past its Core Bridge expiration time.
    ExpiredGuardianSet = 12,
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
    /// Governance VAA emitter is not `(chain=1, GOVERNANCE_EMITTER)`.
    InvalidGovernanceEmitter = 21,
    /// Governance payload module differs from the module the handler expects.
    InvalidGovernanceModule = 22,
    /// Governance payload action byte differs from the handler's action.
    InvalidGovernanceAction = 23,
    /// Governance target chain is not one the handler accepts (`0x0000` and/or Solana).
    GovernanceChainMismatch = 24,
    /// `modify_balance` `kind` byte is neither `1` nor `2`.
    InvalidModificationKind = 25,
    /// `modify_balance` `Add` overflow.
    ModifyBalanceOverflow = 26,
    /// `modify_balance` `Subtract` underflow, including on an uninitialised PDA.
    ModifyBalanceUnderflow = 27,
    /// `ModifyBalance` PDA already exists for this sequence.
    DuplicateModifyBalance = 28,
    /// WTT only. Token Bridge action byte is not `0x01`, `0x02`, or `0x03`.
    UnknownTokenBridgePayload = 29,
    /// `GuardianSet` has more keys than `PendingObservationsLayout::MAX_GUARDIANS`.
    GuardianSetTooLarge = 30,
    /// `RegisterChain` PDA already exists for this sequence.
    DuplicateRegisterChain = 31,
    /// WTT only. Token Bridge transfer payload exceeds `MAX_TRANSFER_PAYLOAD_LEN`.
    TransferPayloadTooLarge = 32,
    /// NTT only. Transceiver message (transfer, hub or peer registration) failed to parse.
    MalformedNttMessage = 33,
    /// NTT only. Standard Relayer `DeliveryInstruction` failed to parse.
    MalformedDeliveryInstruction = 34,
    /// NTT only. Transceiver message or relayer delivery exceeds `MAX_NTT_PAYLOAD_LEN`.
    NttPayloadTooLarge = 35,
    /// NTT only. `register_hub` info message is Burning mode; only Locking registers a hub.
    NotLockingHub = 36,
    /// NTT only. `TransceiverHub` PDA already exists for this transceiver.
    DuplicateTransceiverHub = 37,
    /// NTT only. No `TransceiverHub` PDA for the transceiver; in `register_peer`, neither the
    /// sender nor the peer has one.
    MissingTransceiverHub = 38,
    /// NTT only. `TransceiverPeer` PDA already exists for this transceiver and chain.
    DuplicateTransceiverPeer = 39,
    /// NTT only. The peer's hub differs from this transceiver's registered hub.
    PeerRegistrationMismatch = 40,
    /// NTT only. The sender has no hub and the peer's hub is not the peer itself.
    PeerBeforeHub = 41,
    /// NTT only. `register_peer` names a peer on the sender's own chain.
    SameChainPeer = 42,
    /// NTT only. The hub has no `TransceiverPeer` entry naming the sender on its chain, so the
    /// sender may not adopt it.
    HubHasNotRegisteredPeer = 43,
    /// NTT only. Only a hub (self-referential entry) may register a peer that has no hub.
    HublessPeerRequiresHub = 44,
    /// NTT only. The sender has no `TransceiverPeer` entry for the recipient chain.
    MissingSourcePeer = 45,
    /// NTT only. The sender's peer has no `TransceiverPeer` entry for the sender's chain.
    MissingDestinationPeer = 46,
    /// NTT only. The peer's entry for the sender's chain names another transceiver.
    PeersNotCrossRegistered = 47,
}

impl From<GlobalAccountantError> for u32 {
    fn from(e: GlobalAccountantError) -> Self {
        e as u32
    }
}
