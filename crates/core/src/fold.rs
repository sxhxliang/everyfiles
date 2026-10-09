//! Case folding, shared by ingest and by the query path so the two always agree.
//!
//! ASCII takes a branch-light fast path. Non-ASCII goes through the Unicode
//! lowercase mapping from `std`, which approximates simple case folding closely
//! enough for filename search. Full folding (`ß → ss`, `ﬁ → fi`) is deliberately
//! not applied; it changes string length in ways the offset mapping is not
//! built for, and is a documented gap.
//!
//! Invalid UTF-8 (possible in filenames on Linux) is preserved byte-for-byte:
//! only the valid chunks are folded, so a folded name can never fail to
//! round-trip the bytes the filesystem gave us.

/// `true` if every byte is ASCII and none is an uppercase letter.
#[inline]
pub fn is_lower_ascii(s: &[u8]) -> bool {
    s.iter().all(|&b| b < 0x80 && !b.is_ascii_uppercase())
}

/// Append the folded form of `src` to `dst`.
pub fn fold_into(src: &[u8], dst: &mut Vec<u8>) {
    if src.is_ascii() {
        dst.extend(src.iter().map(u8::to_ascii_lowercase));
        return;
    }
    for chunk in src.utf8_chunks() {
        for ch in chunk.valid().chars() {
            if ch.is_ascii() {
                dst.push(ch.to_ascii_lowercase() as u8);
            } else {
                let mut buf = [0u8; 4];
                for lc in ch.to_lowercase() {
                    dst.extend_from_slice(lc.encode_utf8(&mut buf).as_bytes());
                }
            }
        }
        dst.extend_from_slice(chunk.invalid());
    }
}

/// Convenience: fold into a fresh `Vec`.
pub fn fold(src: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(src.len());
    fold_into(src, &mut v);
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_lowercases() {
        assert_eq!(fold(b"Hello.RS"), b"hello.rs");
        assert_eq!(fold(b"already"), b"already");
        assert_eq!(fold(b""), b"");
    }

    #[test]
    fn non_ascii_lowercases_and_can_change_length() {
        assert_eq!(fold("ÄÖÜ".as_bytes()), "äöü".as_bytes());
        assert_eq!(fold("Straße".as_bytes()), "straße".as_bytes());
        // KELVIN SIGN (3 bytes) lowercases to ASCII `k` (1 byte): this is why
        // fold_off/fold_len are stored separately from name_off/name_len.
        assert_eq!(fold("\u{212A}".as_bytes()), b"k");
    }

    #[test]
    fn invalid_utf8_is_preserved() {
        let src = b"Ab\xff\xfeCd";
        assert_eq!(fold(src), b"ab\xff\xfecd");
    }

    #[test]
    fn idempotent() {
        for s in ["MiXeD", "ÄÖÜ", "日本語", "x\u{212A}y"] {
            let once = fold(s.as_bytes());
            assert_eq!(fold(&once), once);
        }
    }

    #[test]
    fn is_lower_ascii_matches_definition() {
        assert!(is_lower_ascii(b"abc.rs"));
        assert!(!is_lower_ascii(b"Abc"));
        assert!(!is_lower_ascii("é".as_bytes()));
    }
}
