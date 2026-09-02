//! Raw-bytes instruction-argument wrapper.
//!
//! Identical to the sibling WTT `global-accountant` crate's
//! `raw_ix_data::RawIxData` (see that module's doc for the full rationale):
//! Anchor's generated per-instruction handler wrapper Borsh-deserializes each
//! declared argument from the instruction data left after stripping the
//! discriminator. A plain `Vec<u8>` argument would impose Borsh's own 4-byte
//! little-endian length prefix on top of our existing custom framing — a
//! wire-format change the migration explicitly rules out (this program's VAA
//! bodies and `bump ‖ len ‖ body` framing must stay byte-identical).
//!
//! `RawIxData` sidesteps this by implementing `AnchorSerialize`/
//! `AnchorDeserialize` by hand: deserializing consumes every remaining byte
//! verbatim, with no length prefix of its own. Declaring each handler's sole
//! argument as `RawIxData` therefore hands the handler body the exact on-wire
//! bytes that followed the 1-byte dispatch discriminator, unchanged.
//!
//! Duplicated here (rather than imported from `global-accountant`) because the
//! two programs are sibling crates with no shared program-level dependency —
//! only `crates/definitions` and `crates/operational-core` are shared. The
//! type is a small, stable leaf wrapper with no further dependencies, so the
//! duplication carries no divergence risk.

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
