//! Minecraft-style VarInt: little-endian base 128, at most 5 bytes.

/// Appends `v` to `out`.
pub fn write(mut v: u32, out: &mut Vec<u8>) {
    loop {
        let byte = (v & 0x7f) as u8;
        v >>= 7;
        if v == 0 {
            out.push(byte);
            return;
        }
        out.push(byte | 0x80);
    }
}

/// Reads a VarInt from the start of `buf`, returning the value and the
/// number of bytes it used. `None` if `buf` ends first or it is over 5 bytes.
pub fn read(buf: &[u8]) -> Option<(u32, usize)> {
    let mut value = 0u32;
    for (i, &b) in buf.iter().enumerate().take(5) {
        value |= ((b & 0x7f) as u32) << (7 * i);
        if b & 0x80 == 0 {
            return Some((value, i + 1));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        for v in [0u32, 1, 127, 128, 153, 300, 16_383, 16_384, 2_097_151, u32::MAX] {
            let mut buf = Vec::new();
            write(v, &mut buf);
            assert_eq!(read(&buf), Some((v, buf.len())), "value {v}");
        }
    }

    #[test]
    fn two_byte_length_seen_in_captures() {
        // 153-byte Opus frames were written as 0x99 0x01.
        let mut buf = Vec::new();
        write(153, &mut buf);
        assert_eq!(buf, [0x99, 0x01]);
    }

    #[test]
    fn truncated_and_overlong() {
        assert_eq!(read(&[0x80]), None);
        assert_eq!(read(&[0x80; 6]), None);
        assert_eq!(read(&[]), None);
    }
}
