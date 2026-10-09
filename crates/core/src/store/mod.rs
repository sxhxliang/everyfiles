//! The on-disk index format.
//!
//! Flat, little-endian, 8-byte aligned, and zero-deserialization: every
//! section is a plain scalar array, so loading is `mmap` plus bounds and
//! alignment checks, and every full block of every array is served straight
//! from the file. Only a partial tail block is copied so the index can grow.
//!
//! ```text
//! Header (256 B)   magic, version, endian tag, counts, section table
//!                  location, strings location, uuid, payload hash,
//!                  header hash
//! Sections         NAME_POOL FOLD_POOL NAME_OFF NAME_LEN FOLD_OFF FOLD_LEN
//!                  PARENT SIZE MTIME FLAGS STRINGS, each 8-byte aligned
//! Section table    32-byte entries: kind, elem_size, align, off, len
//! ```
//!
//! The strings section holds `[u32 len][bytes]` × 3: the root path, the
//! volume identity (kind byte then bytes), and the watcher resume token.
//!
//! Checksums are xxh3-64. The header hash is checked eagerly; the payload
//! hash covers everything after the header and is checked by
//! [`Loaded::verify_payload`], which a caller may run on a background thread
//! so a cold start never waits on hashing ~80 MB.
//!
//! The format is fixed little-endian and refuses to load on a big-endian
//! host via the endian tag. It also refuses to load an index written on a
//! platform with a different `OsStr` encoding; that is what keeps turning
//! stored name bytes back into paths sound.

mod header;
mod load;
mod write;

pub use header::{FORMAT_VERSION, HEADER_LEN, Header, MAGIC};
pub use load::{Loaded, load};
pub use write::{SaveMeta, SaveStats, save};

/// Identifiers in the section table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub(crate) enum SectionKind {
    NamePool = 1,
    FoldPool = 2,
    NameOff = 3,
    NameLen = 4,
    FoldOff = 5,
    FoldLen = 6,
    Parent = 7,
    Size = 8,
    Mtime = 9,
    Flags = 10,
    Strings = 11,
}

pub(crate) const SECTION_ENTRY_LEN: usize = 32;

/// One section-table entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Section {
    pub kind: u32,
    pub elem_size: u32,
    pub align: u32,
    pub off: u64,
    pub len: u64,
}

impl Section {
    pub fn encode(&self) -> [u8; SECTION_ENTRY_LEN] {
        let mut b = [0u8; SECTION_ENTRY_LEN];
        b[0..4].copy_from_slice(&self.kind.to_le_bytes());
        b[4..8].copy_from_slice(&self.elem_size.to_le_bytes());
        b[8..12].copy_from_slice(&self.align.to_le_bytes());
        b[16..24].copy_from_slice(&self.off.to_le_bytes());
        b[24..32].copy_from_slice(&self.len.to_le_bytes());
        b
    }

    pub fn decode(b: &[u8]) -> Self {
        let u32_at = |i: usize| u32::from_le_bytes(b[i..i + 4].try_into().unwrap());
        let u64_at = |i: usize| u64::from_le_bytes(b[i..i + 8].try_into().unwrap());
        Self {
            kind: u32_at(0),
            elem_size: u32_at(4),
            align: u32_at(8),
            off: u64_at(16),
            len: u64_at(24),
        }
    }
}

/// Header flag: names are WTF-8 (Windows) rather than raw bytes.
pub(crate) const FLAG_WTF8: u32 = 1 << 0;

/// Round up to a multiple of 8.
#[inline]
pub(crate) fn align8(n: u64) -> u64 {
    (n + 7) & !7
}

/// Stable 64-bit fingerprint of arbitrary bytes (xxh3-64).
pub fn fingerprint(bytes: &[u8]) -> u64 {
    xxhash_rust::xxh3::xxh3_64(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn section_round_trip() {
        let s = Section {
            kind: 7,
            elem_size: 4,
            align: 4,
            off: 0x1234_5678_9abc,
            len: 42,
        };
        assert_eq!(Section::decode(&s.encode()), s);
    }

    #[test]
    fn align8_rounds_up() {
        assert_eq!(align8(0), 0);
        assert_eq!(align8(1), 8);
        assert_eq!(align8(8), 8);
        assert_eq!(align8(9), 16);
    }
}
