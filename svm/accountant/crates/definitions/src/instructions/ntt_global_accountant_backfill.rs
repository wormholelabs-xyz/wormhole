//! NTT global-accountant backfill instruction discriminators. Namespaced: every program names
//! its enum `Instruction`. Numbering mirrors the WTT backfill for the shared instructions.

/// Single-byte prefix on the instruction data.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Instruction {
    /// Flip NoReplay bits for a batch of `(chain, emitter, sequence)` and emit one `ACCDGST\0`
    /// commit log per entry.
    BackfillNoReplay = 0,
    /// Write `BalanceAccount` PDAs from the wormchain snapshot rows.
    BackfillBalance = 1,
    /// Write `ModifyBalance` record PDAs, arming `modify_balance`'s replay guard.
    BackfillModifyBalance = 2,
    /// Write the Standard Relayer `ChainRegistration` PDA and the `RegisterChain` record PDA
    /// per registration, both under the NTT program id. Shares the handler and the wire format
    /// with the WTT backfill's `BackfillChainRegistration`. The source module is the one
    /// difference: these registrations are the Wormhole Relayer module's, not the Token Bridge
    /// module's.
    BackfillRelayerChainRegistration = 3,
    /// Write `TransceiverHub` PDAs from the `transceiver_to_hub` map.
    BackfillTransceiverHub = 4,
    /// Write `TransceiverPeer` PDAs from the `transceiver_peers` map.
    BackfillTransceiverPeer = 5,
}

impl Instruction {
    pub const fn from_u8(value: u8) -> Option<Self> {
        match value {
            0 => Some(Self::BackfillNoReplay),
            1 => Some(Self::BackfillBalance),
            2 => Some(Self::BackfillModifyBalance),
            3 => Some(Self::BackfillRelayerChainRegistration),
            4 => Some(Self::BackfillTransceiverHub),
            5 => Some(Self::BackfillTransceiverPeer),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_u8_covers_every_discriminator() {
        let cases: [(u8, Option<Instruction>); 7] = [
            (0, Some(Instruction::BackfillNoReplay)),
            (1, Some(Instruction::BackfillBalance)),
            (2, Some(Instruction::BackfillModifyBalance)),
            (3, Some(Instruction::BackfillRelayerChainRegistration)),
            (4, Some(Instruction::BackfillTransceiverHub)),
            (5, Some(Instruction::BackfillTransceiverPeer)),
            (6, None),
        ];
        for (value, expected) in cases {
            assert_eq!(Instruction::from_u8(value), expected, "{value}");
        }
    }
}
