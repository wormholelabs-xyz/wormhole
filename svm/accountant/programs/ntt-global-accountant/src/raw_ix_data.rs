//! Raw instruction-argument wrapper. A `Vec<u8>` argument would add Borsh's 4-byte length
//! prefix to the wire format; `RawIxData` reads every remaining byte as-is.

use anchor_lang::prelude::*;
use borsh::io::{Read, Write};

/// Instruction data after the 1-byte discriminator.
#[derive(Clone, Debug, Default)]
pub struct RawIxData(pub Vec<u8>);

impl AnchorSerialize for RawIxData {
    fn serialize<W: Write>(&self, writer: &mut W) -> borsh::io::Result<()> {
        writer.write_all(&self.0)
    }
}

impl AnchorDeserialize for RawIxData {
    fn deserialize_reader<R: Read>(reader: &mut R) -> borsh::io::Result<Self> {
        let mut buf = Vec::new();
        reader.read_to_end(&mut buf)?;
        Ok(RawIxData(buf))
    }
}
