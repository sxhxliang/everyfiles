//! Zero-copy load.

use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use memmap2::Mmap;

use super::header::{HEADER_LEN, Header};
use super::{FLAG_WTF8, SECTION_ENTRY_LEN, Section, SectionKind};
use crate::error::{Error, Result};
use crate::index::{Chunked, Index, NamePool, Parts};
use crate::platform::{NameEncoding, VolumeConfig};
use crate::scan::{VolumeId, VolumeIdKind, os_str_from_bytes};

/// A loaded index plus the metadata stored with it.
#[derive(Debug)]
pub struct Loaded {
    pub index: Index,
    pub volume_id: VolumeId,
    pub resume_token: Vec<u8>,
    pub generated_unix_nanos: i64,
    pub file_len: u64,
    map: Arc<Mmap>,
    header: Header,
}

impl Loaded {
    /// Check the payload hash. Covers every byte after the header; callers
    /// may run this on a background thread.
    pub fn verify_payload(&self) -> bool {
        xxhash_rust::xxh3::xxh3_64(&self.map[HEADER_LEN..]) == self.header.payload_hash
    }
}

/// Map `path` and build an index over it.
pub fn load(path: &Path, cfg: VolumeConfig) -> Result<Loaded> {
    let file = File::open(path).map_err(|e| Error::io(path, e))?;
    // SAFETY: the file is opened read-only and the mapping is never written.
    // Another process truncating or rewriting the file underneath us is the
    // one hazard; every offset below is bounds-checked against the mapped
    // length before use, and the payload hash catches silent rewrites.
    let map = unsafe { Mmap::map(&file) }.map_err(|e| Error::io(path, e))?;
    let map = Arc::new(map);
    let header = Header::decode(&map)?;

    let wtf8 = header.flags & FLAG_WTF8 != 0;
    let host_wtf8 = cfg.encoding == NameEncoding::Wtf8;
    if wtf8 != host_wtf8 {
        return Err(Error::Incompatible(
            "index was written on a platform with a different path encoding".into(),
        ));
    }

    // Section table.
    let tbl_off = usize::try_from(header.section_table_off).map_err(|_| corrupt("table offset"))?;
    let tbl_len = header.section_count as usize * SECTION_ENTRY_LEN;
    let tbl = slice(&map, tbl_off, tbl_len, "section table")?;
    let mut by_kind: [Option<Section>; 12] = [None; 12];
    for e in tbl.as_chunks::<SECTION_ENTRY_LEN>().0 {
        let s = Section::decode(e);
        if (1..12).contains(&s.kind) {
            by_kind[s.kind as usize] = Some(s);
        }
    }
    let sect = |k: SectionKind| -> Result<Section> {
        by_kind[k as usize].ok_or_else(|| corrupt(&format!("missing section {k:?}")))
    };

    let n = usize::try_from(header.record_count).map_err(|_| corrupt("record count"))?;

    // Pools.
    let name_pool = pool(&map, sect(SectionKind::NamePool)?, header.name_pool_len)?;
    let fold_pool = pool(&map, sect(SectionKind::FoldPool)?, header.fold_pool_len)?;

    // Arrays.
    let name_off = array::<u32>(&map, sect(SectionKind::NameOff)?, n)?;
    let name_len = array::<u16>(&map, sect(SectionKind::NameLen)?, n)?;
    let fold_off = array::<u32>(&map, sect(SectionKind::FoldOff)?, n)?;
    let fold_len = array::<u16>(&map, sect(SectionKind::FoldLen)?, n)?;
    let parent = array::<u32>(&map, sect(SectionKind::Parent)?, n)?;
    let size = array::<u64>(&map, sect(SectionKind::Size)?, n)?;
    let mtime = array::<i64>(&map, sect(SectionKind::Mtime)?, n)?;
    let flags = array::<u8>(&map, sect(SectionKind::Flags)?, n)?;

    // Strings.
    let s = sect(SectionKind::Strings)?;
    let sb = slice(&map, s.off as usize, s.len as usize, "strings")?;
    let mut cur = 0usize;
    let mut next = || -> Result<&[u8]> {
        let l = sb
            .get(cur..cur + 4)
            .map(|b| u32::from_le_bytes(b.try_into().unwrap()) as usize)
            .ok_or_else(|| corrupt("strings"))?;
        let v = sb
            .get(cur + 4..cur + 4 + l)
            .ok_or_else(|| corrupt("strings"))?;
        cur += 4 + l;
        Ok(v)
    };
    let root_bytes = next()?;
    let vid_bytes = next()?;
    let resume_token = next()?.to_vec();
    if root_bytes.is_empty() {
        return Err(corrupt("empty root path"));
    }
    // SAFETY: the encoding flag matched this host above, so these bytes were
    // produced by `name_bytes` on a platform with the same `OsStr` encoding.
    let root_path = PathBuf::from(unsafe { os_str_from_bytes(root_bytes) });
    let volume_id = match vid_bytes.split_first() {
        Some((&k, rest)) => VolumeId {
            kind: VolumeIdKind::from_u8(k),
            bytes: rest.to_vec(),
        },
        None => VolumeId::none(),
    };

    if header.live_count > header.record_count {
        return Err(corrupt("live count exceeds record count"));
    }
    let parts = Parts {
        name_pool,
        fold_pool,
        name_off,
        name_len,
        fold_off,
        fold_len,
        parent,
        size,
        mtime,
        flags,
        tombstones: header.record_count - header.live_count,
    };
    let index = Index::from_parts(cfg, root_path, parts);
    Ok(Loaded {
        index,
        volume_id,
        resume_token,
        generated_unix_nanos: header.generated_unix_nanos,
        file_len: map.len() as u64,
        map,
        header,
    })
}

fn corrupt(what: &str) -> Error {
    Error::Corrupt(format!("bad {what}"))
}

fn slice<'a>(map: &'a Mmap, off: usize, len: usize, what: &str) -> Result<&'a [u8]> {
    off.checked_add(len)
        .filter(|&end| end <= map.len())
        .map(|end| &map[off..end])
        .ok_or_else(|| corrupt(&format!("{what} range {off}+{len} in {}", map.len())))
}

fn pool(map: &Arc<Mmap>, s: Section, declared: u64) -> Result<NamePool> {
    if s.len != declared {
        return Err(corrupt("pool length"));
    }
    let bytes = slice(map, s.off as usize, s.len as usize, "pool")?;
    // SAFETY: `bytes` is a bounds-checked view of the read-only mapping, and
    // the pool keeps the `Arc<Mmap>` alive for as long as it exists.
    Ok(unsafe { NamePool::from_mapped(bytes.as_ptr(), bytes.len(), Arc::clone(map)) })
}

fn array<T: Copy + Default>(map: &Arc<Mmap>, s: Section, n: usize) -> Result<Chunked<T>> {
    let want = (n as u64) * size_of::<T>() as u64;
    if s.elem_size as usize != size_of::<T>() || s.len != want {
        return Err(corrupt("array section size"));
    }
    let bytes = slice(map, s.off as usize, s.len as usize, "array")?;
    let ptr = bytes.as_ptr();
    if ptr.align_offset(align_of::<T>()) != 0 {
        return Err(corrupt("array alignment"));
    }
    // SAFETY: bounds and alignment checked above; the mapping is read-only
    // and kept alive by the `Arc` handed to the array. Every `T` here is a
    // plain integer, so any byte pattern is a valid value.
    Ok(unsafe { Chunked::from_mapped(ptr.cast::<T>(), n, Arc::clone(map)) })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::{BLOCK, flags};
    use crate::query::{Query, search};
    use crate::scan::VolumeIdKind;
    use crate::store::{SaveMeta, save};

    fn build(n: usize) -> Index {
        let mut idx = Index::new(VolumeConfig::unix(), PathBuf::from("/data/root"), Some(42));
        let mut last_dir = 0u32;
        for k in 0..n {
            let (name, f) = if k % 50 == 0 {
                (format!("Dir{k}"), flags::DIR | flags::HAS_MTIME)
            } else {
                (format!("File_{k}.RS"), flags::HAS_SIZE | flags::HAS_MTIME)
            };
            let id = idx
                .push_record(name.as_bytes(), last_dir, f, k as u64 * 3, k as i64)
                .unwrap();
            if f & flags::DIR != 0 {
                last_dir = id;
            }
        }
        if idx.len() > 3 {
            idx.set_tombstone(3);
        }
        idx
    }

    fn same(a: &Index, b: &Index) {
        assert_eq!(a.len(), b.len());
        assert_eq!(a.live_count(), b.live_count());
        assert_eq!(a.root_path(), b.root_path());
        for id in 0..a.len() as u32 {
            assert_eq!(a.name(id), b.name(id), "name {id}");
            assert_eq!(a.folded(id), b.folded(id), "folded {id}");
            assert_eq!(a.parent(id), b.parent(id), "parent {id}");
            assert_eq!(a.size(id), b.size(id), "size {id}");
            assert_eq!(a.mtime(id), b.mtime(id), "mtime {id}");
            assert_eq!(a.flags(id), b.flags(id), "flags {id}");
        }
    }

    #[test]
    fn round_trip_small_and_across_blocks() {
        for n in [1usize, 10, BLOCK + 7, BLOCK * 2] {
            let idx = build(n);
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("t.idx");
            let vid = VolumeId {
                kind: VolumeIdKind::Generic,
                bytes: vec![1, 2, 3],
            };
            let st = save(
                &idx,
                &path,
                &SaveMeta {
                    volume_id: &vid,
                    resume_token: b"tok",
                },
            )
            .unwrap();
            assert_eq!(st.bytes, std::fs::metadata(&path).unwrap().len());
            let l = load(&path, VolumeConfig::unix()).unwrap();
            assert!(l.verify_payload());
            assert_eq!(l.volume_id, vid);
            assert_eq!(l.resume_token, b"tok");
            same(&idx, &l.index);
            // queries agree
            let q = Query::new("file_1");
            assert_eq!(
                search(&idx, &q).unwrap().ids,
                search(&l.index, &q).unwrap().ids
            );
            // full blocks are served from the map, not copied
            if n >= BLOCK {
                assert!(l.index.mapped_bytes() > 0, "n={n}");
            }
        }
    }

    #[test]
    fn loaded_index_can_grow_and_tombstone() {
        let idx = build(BLOCK + 5);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.idx");
        save(
            &idx,
            &path,
            &SaveMeta {
                volume_id: &VolumeId::none(),
                resume_token: &[],
            },
        )
        .unwrap();
        let mut l = load(&path, VolumeConfig::unix()).unwrap();
        let before = l.index.len();
        let id = l
            .index
            .push_record(b"new.txt", 0, flags::HAS_SIZE, 1, 1)
            .unwrap();
        assert_eq!(id as usize, before);
        assert_eq!(l.index.name(id), b"new.txt");
        // tombstone inside a mapped block: copy-on-write, file untouched
        l.index.set_tombstone(1);
        assert!(l.index.is_tombstone(1));
        let again = load(&path, VolumeConfig::unix()).unwrap();
        assert!(!again.index.is_tombstone(1));
        assert!(again.verify_payload());
    }

    #[test]
    fn payload_corruption_is_detected_by_verify() {
        let idx = build(100);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.idx");
        save(
            &idx,
            &path,
            &SaveMeta {
                volume_id: &VolumeId::none(),
                resume_token: &[],
            },
        )
        .unwrap();
        let mut bytes = std::fs::read(&path).unwrap();
        let i = HEADER_LEN + 40;
        bytes[i] ^= 0xff;
        std::fs::write(&path, &bytes).unwrap();
        let l = load(&path, VolumeConfig::unix()).unwrap();
        assert!(!l.verify_payload());
    }

    #[test]
    fn truncated_file_is_rejected() {
        let idx = build(100);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.idx");
        save(
            &idx,
            &path,
            &SaveMeta {
                volume_id: &VolumeId::none(),
                resume_token: &[],
            },
        )
        .unwrap();
        let bytes = std::fs::read(&path).unwrap();
        std::fs::write(&path, &bytes[..bytes.len() / 2]).unwrap();
        assert!(matches!(
            load(&path, VolumeConfig::unix()),
            Err(Error::Corrupt(_))
        ));
        std::fs::write(&path, &bytes[..100]).unwrap();
        assert!(matches!(
            load(&path, VolumeConfig::unix()),
            Err(Error::Corrupt(_))
        ));
    }

    #[test]
    fn foreign_encoding_is_incompatible() {
        let idx = build(10);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.idx");
        save(
            &idx,
            &path,
            &SaveMeta {
                volume_id: &VolumeId::none(),
                resume_token: &[],
            },
        )
        .unwrap();
        assert!(matches!(
            load(&path, VolumeConfig::windows()),
            Err(Error::Incompatible(_))
        ));
    }

    #[test]
    fn save_is_atomic_leaves_no_temp_files() {
        let idx = build(10);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.idx");
        save(
            &idx,
            &path,
            &SaveMeta {
                volume_id: &VolumeId::none(),
                resume_token: &[],
            },
        )
        .unwrap();
        let names: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, ["t.idx"]);
    }
}
