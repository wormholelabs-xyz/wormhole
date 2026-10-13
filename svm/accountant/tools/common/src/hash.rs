//! Content hash of the snapshot file.

use std::fs::File;
use std::path::Path;

use sha2::{Digest, Sha256};

/// SHA-256 of a file, streamed. The progress file identity and the parity ledger
/// both record this hash of the snapshot.
pub fn file_sha256(path: &Path) -> std::io::Result<[u8; 32]> {
    let mut hasher = Sha256::new();
    std::io::copy(&mut File::open(path)?, &mut hasher)?;
    Ok(hasher.finalize().into())
}

/// `0x`-prefixed lowercase hex of 32 bytes.
pub fn hex32(bytes: &[u8; 32]) -> String {
    format!("0x{}", hex::encode(bytes))
}
