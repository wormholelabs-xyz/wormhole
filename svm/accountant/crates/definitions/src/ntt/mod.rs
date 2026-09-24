//! NTT wire formats and amount normalization, as the CosmWasm NTT accountant. Token identity
//! comes from the hub registry, so parsers skip it. Prefixes per the NTT contracts
//! (`TransceiverStructs.sol`, `WormholeTransceiverState.sol`) and `ntt-messages`.

pub mod amount;
pub mod registration;
pub mod transfer;

pub use amount::*;
pub use registration::*;
pub use transfer::*;

/// Cap on a transceiver message or relayer delivery
pub const MAX_NTT_PAYLOAD_LEN: usize = 2000;
