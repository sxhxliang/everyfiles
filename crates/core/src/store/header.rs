//! The fixed 256-byte file header.

use crate::error::{Error, Result};

pub const MAGIC: [u8; 8] = *b"EVFILES\0";
pub const FORMAT_VERSION: u32 = 1;
pub const HEADER_LEN: usize = 256;
/// Written as `0x01020304`; a big-endian reader sees `0x04030201` and refuses.
pub const ENDIAN_TAG: u32 = 0x0102_0304;

/// Byte range the header hash covers (everything before the hash field).
const HASHED_LEN: usize = 120;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Header {
    pub flags: u32,
    pub record_count: u64,
    pub live_count: u64,
    pub name_pool_len: u64,
    pub fold_pool_len: u64,
    pub generated_unix_nanos: i64,
    pub section_table_off: u64,
    pub section_count: u32,
    pub strings_off: u64,
    pub strings_len: u64,
    pub index_uuid: [u8; 16],
    pub payload_hash: u64,
}

impl Header {
    pub fn encode(&self) -> [u8; HEADER_LEN] {
        let mut b = [0u8; HEADER_LEN];
        b[0..8].copy_from_slice(&MAGIC);
        b[8..12].copy_from_slice(&FORMAT_VERSION.to_le_bytes());
        b[12..16].copy_from_slice(&(HEADER_LEN as u32).to_le_bytes());
        b[16..20].copy_from_slice(&ENDIAN_TAG.to_le_bytes());
        b[20..24].copy_from_slice(&self.flags.to_le_bytes());
        b[24..32].copy_from_slice(&self.record_count.to_le_bytes());
        b[32..40].copy_from_slice(&self.live_count.to_le_bytes());
        b[40..48].copy_from_slice(&self.name_pool_len.to_le_bytes());
        b[48..56].copy_from_slice(&self.fold_pool_len.to_le_bytes());
        b[56..64].copy_from_slice(&self.generated_unix_nanos.to_le_bytes());
        b[64..72].copy_from_slice(&self.section_table_off.to_le_bytes());
        b[72..76].copy_from_slice(&self.section_count.to_le_bytes());
        b[80..88].copy_from_slice(&self.strings_off.to_le_bytes());
        b[88..96].copy_from_slice(&self.strings_len.to_le_bytes());
        b[96..112].copy_from_slice(&self.index_uuid);
        b[112..120].copy_from_slice(&self.payload_hash.to_le_bytes());
        let h = xxhash_rust::xxh3::xxh3_64(&b[..HASHED_LEN]);
        b[120..128].copy_from_slice(&h.to_le_bytes());
        b
    }

    /// Parse and validate. Distinguishes "not one of ours / wrong version"
    /// ([`Error::Incompatible`]) from "damaged" ([`Error::Corrupt`]).
    pub fn decode(b: &[u8]) -> Result<Self> {
        if b.len() < HEADER_LEN {
            return Err(Error::Corrupt(format!(
                "file shorter than header ({} bytes)",
                b.len()
            )));
        }
        if b[0..8] != MAGIC {
            return Err(Error::Incompatible(
                "bad magic; not an everyfiles index".into(),
            ));
        }
        let u32_at = |i: usize| u32::from_le_bytes(b[i..i + 4].try_into().unwrap());
        let u64_at = |i: usize| u64::from_le_bytes(b[i..i + 8].try_into().unwrap());
        let version = u32_at(8);
        if version != FORMAT_VERSION {
            return Err(Error::Incompatible(format!(
                "format version {version}, this build reads {FORMAT_VERSION}"
            )));
        }
        if u32_at(16) != ENDIAN_TAG {
            return Err(Error::Incompatible(
                "byte order differs from this host".into(),
            ));
        }
        if u32_at(12) as usize != HEADER_LEN {
            return Err(Error::Corrupt(format!("header length {}", u32_at(12))));
        }
        let want = xxhash_rust::xxh3::xxh3_64(&b[..HASHED_LEN]);
        if u64_at(120) != want {
            return Err(Error::Corrupt("header checksum mismatch".into()));
        }
        let mut index_uuid = [0u8; 16];
        index_uuid.copy_from_slice(&b[96..112]);
        Ok(Self {
            flags: u32_at(20),
            record_count: u64_at(24),
            live_count: u64_at(32),
            name_pool_len: u64_at(40),
            fold_pool_len: u64_at(48),
            generated_unix_nanos: i64::from_le_bytes(b[56..64].try_into().unwrap()),
            section_table_off: u64_at(64),
            section_count: u32_at(72),
            strings_off: u64_at(80),
            strings_len: u64_at(88),
            index_uuid,
            payload_hash: u64_at(112),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Header {
        Header {
            flags: 1,
            record_count: 10,
            live_count: 9,
            name_pool_len: 100,
            fold_pool_len: 101,
            generated_unix_nanos: -5,
            section_table_off: 4096,
            section_count: 11,
            strings_off: 3000,
            strings_len: 50,
            index_uuid: [7u8; 16],
            payload_hash: 0xdead_beef,
        }
    }

    #[test]
    fn round_trip() {
        let h = sample();
        assert_eq!(Header::decode(&h.encode()).unwrap(), h);
    }

    #[test]
    fn every_corrupted_hashed_byte_is_detected() {
        let good = sample().encode();
        for i in 0..HASHED_LEN {
            let mut bad = good;
            bad[i] ^= 0x5a;
            let r = Header::decode(&bad);
            assert!(r.is_err(), "byte {i} flip went undetected");
        }
    }

    #[test]
    fn wrong_version_is_incompatible_not_corrupt() {
        let mut b = sample().encode();
        b[8..12].copy_from_slice(&99u32.to_le_bytes());
        // re-hash so only the version differs
        let h = xxhash_rust::xxh3::xxh3_64(&b[..HASHED_LEN]);
        b[120..128].copy_from_slice(&h.to_le_bytes());
        assert!(matches!(Header::decode(&b), Err(Error::Incompatible(_))));
    }

    #[test]
    fn short_input_is_corrupt() {
        assert!(matches!(Header::decode(&[0u8; 10]), Err(Error::Corrupt(_))));
    }
}
