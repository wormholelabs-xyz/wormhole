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
    /// per registration, both under the NTT program id.
    BackfillRelayerChainRegistration = 3,
}

impl Instruction {
    pub const fn from_u8(value: u8) -> Option<Self> {
        match value {
            0 => Some(Self::BackfillNoReplay),
            1 => Some(Self::BackfillBalance),
            2 => Some(Self::BackfillModifyBalance),
            3 => Some(Self::BackfillRelayerChainRegistration),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_u8_covers_every_discriminator() {
        let cases: [(u8, Option<Instruction>); 5] = [
            (0, Some(Instruction::BackfillNoReplay)),
            (1, Some(Instruction::BackfillBalance)),
            (2, Some(Instruction::BackfillModifyBalance)),
            (3, Some(Instruction::BackfillRelayerChainRegistration)),
            (4, None),
        ];
        for (value, expected) in cases {
            assert_eq!(Instruction::from_u8(value), expected, "{value}");
        }
    }
}
