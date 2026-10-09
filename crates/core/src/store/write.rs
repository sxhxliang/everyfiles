//! Atomic save.
//!
//! Write to a temp file in the same directory, `fsync` it, `rename` over the
//! target, then `fsync` the directory. A crash leaves either the old file or
//! a complete new one, never a torn one.

use std::fs::File;
use std::io::{BufWriter, Seek, SeekFrom, Write};
use std::path::Path;
use std::time::{Duration, Instant, SystemTime};

use xxhash_rust::xxh3::Xxh3;

use super::header::{HEADER_LEN, Header};
use super::{FLAG_WTF8, Section, SectionKind, align8};
use crate::error::{Error, Result};
use crate::index::Index;
use crate::platform::NameEncoding;
use crate::scan::{VolumeId, name_bytes};

/// Metadata persisted alongside the records.
#[derive(Debug, Clone, Copy)]
pub struct SaveMeta<'a> {
    pub volume_id: &'a VolumeId,
    pub resume_token: &'a [u8],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SaveStats {
    pub bytes: u64,
    pub elapsed: Duration,
}

/// Hashes everything written after the header and tracks the offset.
struct Payload<W: Write> {
    inner: W,
    hasher: Xxh3,
    /// Absolute file offset of the next byte.
    pos: u64,
}

impl<W: Write> Payload<W> {
    fn pad_to_8(&mut self) -> std::io::Result<()> {
        let target = align8(self.pos);
        let pad = (target - self.pos) as usize;
        if pad > 0 {
            self.write_all(&[0u8; 8][..pad])?;
        }
        Ok(())
    }
}

impl<W: Write> Write for Payload<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let n = self.inner.write(buf)?;
        self.hasher.update(&buf[..n]);
        self.pos += n as u64;
        Ok(n)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

/// Persist `index` to `path` atomically.
pub fn save(index: &Index, path: &Path, meta: &SaveMeta<'_>) -> Result<SaveStats> {
    let start = Instant::now();
    let dir = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let base = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "index".into());
    let nonce = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let tmp = dir.join(format!(".{base}.tmp.{}.{nonce}", std::process::id()));

    let result = write_to(index, &tmp, meta);
    match result {
        Ok(bytes) => {
            std::fs::rename(&tmp, path).map_err(|e| Error::io(path, e))?;
            // Make the rename itself durable. Not every platform lets a
            // directory be opened for sync; failure here is not fatal.
            if let Ok(d) = File::open(dir) {
                let _ = d.sync_all();
            }
            Ok(SaveStats {
                bytes,
                elapsed: start.elapsed(),
            })
        }
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(e)
        }
    }
}

fn write_to(index: &Index, tmp: &Path, meta: &SaveMeta<'_>) -> Result<u64> {
    let file = File::create(tmp).map_err(|e| Error::io(tmp, e))?;
    let mut w = BufWriter::with_capacity(1 << 20, file);
    w.write_all(&[0u8; HEADER_LEN])?;
    let mut p = Payload {
        inner: w,
        hasher: Xxh3::new(),
        pos: HEADER_LEN as u64,
    };
    let mut sections: Vec<Section> = Vec::with_capacity(11);

    // Pools: concatenated populated prefixes of every block.
    for (kind, pool) in [
        (SectionKind::NamePool, &index.name_pool),
        (SectionKind::FoldPool, &index.fold_pool),
    ] {
        p.pad_to_8()?;
        let off = p.pos;
        for b in 0..pool.num_blocks() {
            p.write_all(pool.block(b))?;
        }
        sections.push(Section {
            kind: kind as u32,
            elem_size: 1,
            align: 1,
            off,
            len: p.pos - off,
        });
    }

    // Arrays: contiguous elements, native (little-endian) layout.
    macro_rules! array {
        ($kind:expr, $arr:expr, $t:ty) => {{
            p.pad_to_8()?;
            let off = p.pos;
            for blk in $arr.blocks() {
                p.write_all(bytemuck::cast_slice::<$t, u8>(blk))?;
            }
            sections.push(Section {
                kind: $kind as u32,
                elem_size: size_of::<$t>() as u32,
                align: align_of::<$t>() as u32,
                off,
                len: p.pos - off,
            });
        }};
    }
    array!(SectionKind::NameOff, index.name_off, u32);
    array!(SectionKind::NameLen, index.name_len, u16);
    array!(SectionKind::FoldOff, index.fold_off, u32);
    array!(SectionKind::FoldLen, index.fold_len, u16);
    array!(SectionKind::Parent, index.parent, u32);
    array!(SectionKind::Size, index.size, u64);
    array!(SectionKind::Mtime, index.mtime, i64);
    array!(SectionKind::Flags, index.flags, u8);

    // Strings.
    p.pad_to_8()?;
    let strings_off = p.pos;
    let mut vid = Vec::with_capacity(1 + meta.volume_id.bytes.len());
    vid.push(meta.volume_id.kind as u8);
    vid.extend_from_slice(&meta.volume_id.bytes);
    for s in [
        name_bytes(index.root_path.as_os_str()),
        vid.as_slice(),
        meta.resume_token,
    ] {
        let n = u32::try_from(s.len()).map_err(|_| Error::Corrupt("string too long".into()))?;
        p.write_all(&n.to_le_bytes())?;
        p.write_all(s)?;
    }
    let strings_len = p.pos - strings_off;
    sections.push(Section {
        kind: SectionKind::Strings as u32,
        elem_size: 1,
        align: 1,
        off: strings_off,
        len: strings_len,
    });

    // Section table.
    p.pad_to_8()?;
    let table_off = p.pos;
    for s in &sections {
        p.write_all(&s.encode())?;
    }
    let total = p.pos;
    let payload_hash = p.hasher.digest();

    let mut flags = 0u32;
    if index.cfg.encoding == NameEncoding::Wtf8 {
        flags |= FLAG_WTF8;
    }
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_nanos()).unwrap_or(i64::MAX))
        .unwrap_or(0);
    let mut index_uuid = [0u8; 16];
    index_uuid[..8].copy_from_slice(&payload_hash.to_le_bytes());
    index_uuid[8..].copy_from_slice(&(now as u64).to_le_bytes());
    let header = Header {
        flags,
        record_count: index.len() as u64,
        live_count: index.live_count(),
        name_pool_len: index.name_pool.bytes_used() as u64,
        fold_pool_len: index.fold_pool.bytes_used() as u64,
        generated_unix_nanos: now,
        section_table_off: table_off,
        section_count: sections.len() as u32,
        strings_off,
        strings_len,
        index_uuid,
        payload_hash,
    };

    let mut file = p
        .inner
        .into_inner()
        .map_err(|e| Error::IoPlain(e.into_error()))?;
    file.seek(SeekFrom::Start(0))?;
    file.write_all(&header.encode())?;
    file.sync_all()?;
    Ok(total)
}
