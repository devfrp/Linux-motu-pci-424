// SPDX-License-Identifier: GPL-2.0-or-later
//! Shared helpers for the reverse-engineering tools in tools/re/.
//!
//! They read the user's own copy of the vendor driver (`vendor/MOTUAW.sys`,
//! git-ignored) or the user-extracted DSP program. Nothing here is ever
//! redistributed; the tools print facts (addresses, offsets, hashes).

/// Read a whole file or exit with a message.
pub fn read_or_exit(path: &str) -> Vec<u8> {
    std::fs::read(path).unwrap_or_else(|e| {
        eprintln!("{path}: {e}");
        std::process::exit(1);
    })
}

/// Parse a number like Python's int(s, 0): 0x.., 0o.., 0b.. or decimal.
pub fn parse_int(s: &str) -> Option<u64> {
    let s = s.trim();
    let lower = s.to_ascii_lowercase();
    if let Some(h) = lower.strip_prefix("0x") {
        u64::from_str_radix(h, 16).ok()
    } else if let Some(o) = lower.strip_prefix("0o") {
        u64::from_str_radix(o, 8).ok()
    } else if let Some(b) = lower.strip_prefix("0b") {
        u64::from_str_radix(b, 2).ok()
    } else {
        s.parse().ok()
    }
}

/// Little-endian dword at byte offset `off`.
pub fn le32(buf: &[u8], off: usize) -> Option<u32> {
    buf.get(off..off + 4)
        .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ints_parse_like_python() {
        assert_eq!(parse_int("0x6fac"), Some(0x6fac));
        assert_eq!(parse_int("0b101"), Some(5));
        assert_eq!(parse_int("42"), Some(42));
        assert_eq!(parse_int("zz"), None);
    }
}
