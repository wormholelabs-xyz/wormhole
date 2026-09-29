//! Anchor `#[program]` boundary items for the accountant programs: `RawIxData` and
//! `flatten_accounts!`. Anchor-free: the serialization traits are borsh's, and the macro
//! expands in the calling program crate.

#![no_std]

extern crate alloc;

use alloc::vec::Vec;
use borsh::io::{Read, Write};

/// Instruction data after the 1-byte discriminator. A `Vec<u8>` argument would add Borsh's
/// 4-byte length prefix to the wire format; `RawIxData` reads every remaining byte as-is.
#[derive(Clone, Debug, Default)]
pub struct RawIxData(pub Vec<u8>);

impl borsh::BorshSerialize for RawIxData {
    fn serialize<W: Write>(&self, writer: &mut W) -> borsh::io::Result<()> {
        writer.write_all(&self.0)
    }
}

impl borsh::BorshDeserialize for RawIxData {
    fn deserialize_reader<R: Read>(reader: &mut R) -> borsh::io::Result<Self> {
        let mut buf = Vec::new();
        reader.read_to_end(&mut buf)?;
        Ok(RawIxData(buf))
    }
}

/// Flatten a `Context`'s `#[derive(Accounts)]` fields, and optionally
/// `ctx.remaining_accounts`, into the positional `Vec<AccountInfo>` handlers take.
/// Field order must match the handler's account list.
#[macro_export]
macro_rules! flatten_accounts {
    ($ctx:expr, [$($field:ident),+ $(,)?]) => {
        vec![$($ctx.accounts.$field.to_account_info()),+]
    };
    ($ctx:expr, [$($field:ident),+ $(,)?], remaining) => {{
        let mut accounts: ::std::vec::Vec<::anchor_lang::prelude::AccountInfo> =
            vec![$($ctx.accounts.$field.to_account_info()),+];
        accounts.extend($ctx.remaining_accounts.iter().cloned());
        accounts
    }};
}

#[cfg(test)]
mod tests {
    use super::RawIxData;
    use alloc::vec::Vec;

    #[test]
    fn raw_ix_data_round_trip() {
        let long: Vec<u8> = (0..=255u8).chain(0..44u8).collect();
        assert_eq!(long.len(), 300);

        let cases: [(&str, Vec<u8>); 3] = [
            ("empty", Vec::new()),
            ("one byte", alloc::vec![0x2a]),
            ("300 bytes", long),
        ];

        for (name, input) in cases {
            let decoded: RawIxData = borsh::from_slice(&input).expect(name);
            assert_eq!(decoded.0, input, "{name}: deserialize");

            let encoded = borsh::to_vec(&decoded).expect(name);
            assert_eq!(encoded.len(), input.len(), "{name}: serialized length");
            assert_eq!(encoded, input, "{name}: serialize");
        }
    }
}
