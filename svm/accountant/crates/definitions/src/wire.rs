//! Big-endian length-prefix helpers: each returns the parsed front and the remaining bytes,
//! or `None` on a short read.

/// Split a big-endian `u16` off the front.
pub fn split_u16_be(bytes: &[u8]) -> Option<(u16, &[u8])> {
    let (front, rest) = bytes.split_at_checked(2)?;
    Some((u16::from_be_bytes([front[0], front[1]]), rest))
}

/// Split a big-endian `u32` off the front.
pub fn split_u32_be(bytes: &[u8]) -> Option<(u32, &[u8])> {
    let (front, rest) = bytes.split_at_checked(4)?;
    Some((
        u32::from_be_bytes([front[0], front[1], front[2], front[3]]),
        rest,
    ))
}

/// Split a `u16`-length-prefixed blob off the front.
pub fn split_u16_be_prefixed(bytes: &[u8]) -> Option<(&[u8], &[u8])> {
    let (len, rest) = split_u16_be(bytes)?;
    rest.split_at_checked(usize::from(len))
}

/// Split a `u32`-length-prefixed blob off the front.
pub fn split_u32_be_prefixed(bytes: &[u8]) -> Option<(&[u8], &[u8])> {
    let (len, rest) = split_u32_be(bytes)?;
    rest.split_at_checked(len as usize)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_and_short_reads() {
        let data = [0x00, 0x03, 0x99, 0x45, 0x10, 0x00, 0x00, 0x00, 0x01, 0xFF];
        let (blob, rest) = split_u16_be_prefixed(&data).expect("3-byte blob");
        assert_eq!(blob, [0x99, 0x45, 0x10]);
        let (blob, rest) = split_u32_be_prefixed(rest).expect("1-byte blob");
        assert_eq!(blob, [0xFF]);
        assert!(rest.is_empty());

        assert_eq!(split_u16_be(&data[..1]), None, "short u16");
        assert_eq!(split_u32_be(&data[..3]), None, "short u32");
        assert_eq!(
            split_u16_be_prefixed(&[0x00, 0x05, 0x01]),
            None,
            "blob past the end"
        );
    }
}
