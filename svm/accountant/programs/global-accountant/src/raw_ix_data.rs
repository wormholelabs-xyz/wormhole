//! Raw-bytes instruction-argument wrapper.
//!
//! Anchor's generated per-instruction handler wrapper Borsh-deserializes each
//! declared argument from the instruction data left after stripping the
//! discriminator (see `anchor-attribute-program`'s `handlers.rs` codegen:
//! `instruction::<Ix>::deserialize(&mut &__ix_data[..])`). A plain `Vec<u8>`
//! argument would impose Borsh's own 4-byte little-endian length prefix on
//! top of our existing custom framing — a wire-format change the migration
//! explicitly rules out (plan §2a/§3: keep the 1-byte instruction dispatch
//! and the existing per-instruction body format byte-identical).
//!
//! `RawIxData` sidesteps this by implementing `AnchorSerialize`/
//! `AnchorDeserialize` (= `borsh`'s own `BorshSerialize`/`BorshDeserialize`
//! traits, per `anchor_lang::{AnchorSerialize, AnchorDeserialize}`) by hand:
//! deserializing consumes every remaining byte verbatim, with no length
//! prefix of its own. Declaring each handler's sole argument as `RawIxData`
//! therefore hands the handler body the exact on-wire bytes that followed the
//! 1-byte dispatch discriminator, unchanged.

use anchor_lang::prelude::*;
use borsh::io::{Read, Write};

/// The remaining raw instruction-data bytes, after Anchor's own 1-byte
/// instruction discriminator has been stripped by the generated dispatch.
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
