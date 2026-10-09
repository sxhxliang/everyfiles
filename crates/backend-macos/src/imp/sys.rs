//! Raw `getattrlistbulk(2)` / `getattrlist(2)` plumbing.
//!
//! Everything here was checked against the SDK's `sys/attr.h` and the vendored
//! `libc` source rather than assumed. Facts that bit:
//!
//! - The return value of `getattrlistbulk` is a **count of entries**, not a
//!   byte count. `0` means end of directory and stays `0` until the fd is
//!   rewound or reopened.
//! - Each returned group starts with a `u32` byte length (8-byte aligned),
//!   then `ATTR_CMN_RETURNED_ATTRS` (an `attribute_set_t`, always first), then
//!   `ATTR_CMN_ERROR` if requested, then the remaining attributes in bit
//!   order within each group (common, then dir, then file), fixed-size fields
//!   first and variable data at the end.
//! - `attrreference_t.attr_dataoffset` is relative to the **attrreference
//!   struct itself**, not the buffer. `attr_length` includes the NUL.
//! - Fields are only 4-byte aligned, so every multi-byte read goes through
//!   `from_ne_bytes` on a byte slice. Parsing therefore needs no `unsafe`.
//! - `libc` omits `ATTR_CMN_ERROR` and the `fsobj_type_t` values.
//! - `volattr` must be 0 for the bulk call.
//!
//! We do not pass `FSOPT_PACK_INVAL_ATTRS`, so the layout of each group is
//! exactly the set of attributes the *returned* mask names; the parser walks
//! by that mask.

use std::ffi::CString;
use std::io;
use std::os::fd::RawFd;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

use everyfiles_core::scan::Kind;

/// Not exported by `libc`; value from `sys/attr.h`.
pub const ATTR_CMN_ERROR: libc::attrgroup_t = 0x2000_0000;

// `fsobj_type_t` values from `sys/vnode.h` (`enum vtype`).
const VREG: u32 = 1;
const VDIR: u32 = 2;
const VLNK: u32 = 5;

/// Bytes of the leading `attribute_set_t` (five `u32`s).
const RETURNED_LEN: usize = 20;

/// The attribute set every `read_dir` requests.
pub fn bulk_request() -> libc::attrlist {
    libc::attrlist {
        bitmapcount: libc::ATTR_BIT_MAP_COUNT,
        reserved: 0,
        commonattr: libc::ATTR_CMN_NAME
            | libc::ATTR_CMN_RETURNED_ATTRS
            | ATTR_CMN_ERROR
            | libc::ATTR_CMN_OBJTYPE
            | libc::ATTR_CMN_MODTIME
            | libc::ATTR_CMN_FILEID,
        volattr: 0,
        dirattr: 0,
        fileattr: libc::ATTR_FILE_DATALENGTH,
        forkattr: 0,
    }
}

/// One `getattrlistbulk` call. Returns the number of entries written to `buf`.
pub fn bulk(fd: RawFd, req: &mut libc::attrlist, buf: &mut [u8]) -> io::Result<usize> {
    // SAFETY: `fd` is an open directory descriptor the caller keeps alive for
    // the call; `req` is a fully initialized `attrlist`; `buf` is writable for
    // exactly `buf.len()` bytes and the kernel writes at most that many.
    let r = unsafe {
        libc::getattrlistbulk(
            fd,
            std::ptr::from_mut(req).cast(),
            buf.as_mut_ptr().cast(),
            buf.len(),
            0,
        )
    };
    if r < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(r as usize)
    }
}

/// A parsed directory entry borrowing from the kernel buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Entry<'a> {
    pub name: &'a [u8],
    pub kind: Option<Kind>,
    pub size: Option<u64>,
    pub mtime: Option<i64>,
    pub fileid: Option<u64>,
    /// Non-zero when the kernel could not stat this entry.
    pub error: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParseError;

/// Walk `count` groups in `buf`, calling `f` for each.
pub fn parse_entries<'a>(
    buf: &'a [u8],
    count: usize,
    mut f: impl FnMut(Entry<'a>),
) -> Result<(), ParseError> {
    let mut pos = 0usize;
    for _ in 0..count {
        let len = u32_at(buf, pos)? as usize;
        if len < 4 + RETURNED_LEN {
            return Err(ParseError);
        }
        let entry = buf.get(pos..pos + len).ok_or(ParseError)?;
        f(parse_one(entry)?);
        pos += len;
    }
    Ok(())
}

fn parse_one(entry: &[u8]) -> Result<Entry<'_>, ParseError> {
    let mut cur = 4usize;
    let common = u32_at(entry, cur)?;
    let fileattr = u32_at(entry, cur + 12)?;
    cur += RETURNED_LEN;

    let mut e = Entry {
        name: &[],
        kind: None,
        size: None,
        mtime: None,
        fileid: None,
        error: 0,
    };
    if common & ATTR_CMN_ERROR != 0 {
        e.error = u32_at(entry, cur)?;
        cur += 4;
    }
    if common & libc::ATTR_CMN_NAME != 0 {
        let off = i32_at(entry, cur)?;
        let n = u32_at(entry, cur + 4)? as usize;
        let start = usize::try_from(cur as i64 + off as i64).map_err(|_| ParseError)?;
        let raw = entry.get(start..start + n).ok_or(ParseError)?;
        // `attr_length` counts the NUL; be tolerant if it is absent.
        e.name = match raw.iter().position(|&b| b == 0) {
            Some(z) => &raw[..z],
            None => raw,
        };
        cur += 8;
    } else {
        return Err(ParseError); // NAME is mandatory; its absence means confusion
    }
    if common & libc::ATTR_CMN_OBJTYPE != 0 {
        e.kind = Some(match u32_at(entry, cur)? {
            VREG => Kind::File,
            VDIR => Kind::Dir,
            VLNK => Kind::Symlink,
            _ => Kind::Other,
        });
        cur += 4;
    }
    if common & libc::ATTR_CMN_MODTIME != 0 {
        let sec = i64_at(entry, cur)?;
        let nsec = i64_at(entry, cur + 8)?;
        e.mtime = Some(sec.saturating_mul(1_000_000_000).saturating_add(nsec));
        cur += 16;
    }
    if common & libc::ATTR_CMN_FILEID != 0 {
        e.fileid = Some(u64_at(entry, cur)?);
        cur += 8;
    }
    if fileattr & libc::ATTR_FILE_DATALENGTH != 0 {
        e.size = Some(u64_at(entry, cur)?);
    }
    Ok(e)
}

#[inline]
fn u32_at(b: &[u8], i: usize) -> Result<u32, ParseError> {
    b.get(i..i + 4)
        .map(|s| u32::from_ne_bytes(s.try_into().unwrap()))
        .ok_or(ParseError)
}
#[inline]
fn i32_at(b: &[u8], i: usize) -> Result<i32, ParseError> {
    u32_at(b, i).map(|v| v as i32)
}
#[inline]
fn u64_at(b: &[u8], i: usize) -> Result<u64, ParseError> {
    b.get(i..i + 8)
        .map(|s| u64::from_ne_bytes(s.try_into().unwrap()))
        .ok_or(ParseError)
}
#[inline]
fn i64_at(b: &[u8], i: usize) -> Result<i64, ParseError> {
    u64_at(b, i).map(|v| v as i64)
}

/// The UUID of the volume containing `path`, via `getattrlist(2)`.
pub fn volume_uuid(path: &Path) -> io::Result<[u8; 16]> {
    let c = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains NUL"))?;
    let mut req = libc::attrlist {
        bitmapcount: libc::ATTR_BIT_MAP_COUNT,
        reserved: 0,
        commonattr: libc::ATTR_CMN_RETURNED_ATTRS,
        // `ATTR_VOL_INFO` must accompany any other volume attribute.
        volattr: libc::ATTR_VOL_INFO | libc::ATTR_VOL_UUID,
        dirattr: 0,
        fileattr: 0,
        forkattr: 0,
    };
    let mut buf = [0u8; 64];
    // SAFETY: `c` is a valid NUL-terminated path; `req` is initialized; `buf`
    // is writable for its full length, which the kernel does not exceed.
    let r = unsafe {
        libc::getattrlist(
            c.as_ptr(),
            std::ptr::from_mut(&mut req).cast(),
            buf.as_mut_ptr().cast(),
            buf.len(),
            0,
        )
    };
    if r != 0 {
        return Err(io::Error::last_os_error());
    }
    // [u32 len][attribute_set_t][uuid_t]
    let volattr = u32::from_ne_bytes(buf[8..12].try_into().unwrap());
    if volattr & libc::ATTR_VOL_UUID == 0 {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "volume has no UUID",
        ));
    }
    let mut uuid = [0u8; 16];
    uuid.copy_from_slice(&buf[4 + RETURNED_LEN..4 + RETURNED_LEN + 16]);
    Ok(uuid)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build one group exactly as the kernel lays it out.
    fn group(
        name: &[u8],
        kind: u32,
        mtime: (i64, i64),
        fileid: u64,
        size: Option<u64>,
        err: u32,
    ) -> Vec<u8> {
        let mut fixed = Vec::new();
        let mut common = libc::ATTR_CMN_RETURNED_ATTRS
            | libc::ATTR_CMN_NAME
            | libc::ATTR_CMN_OBJTYPE
            | libc::ATTR_CMN_MODTIME
            | libc::ATTR_CMN_FILEID;
        if err != 0 {
            common |= ATTR_CMN_ERROR;
        }
        let fileattr = if size.is_some() {
            libc::ATTR_FILE_DATALENGTH
        } else {
            0
        };
        fixed.extend_from_slice(&common.to_ne_bytes());
        fixed.extend_from_slice(&0u32.to_ne_bytes());
        fixed.extend_from_slice(&0u32.to_ne_bytes());
        fixed.extend_from_slice(&fileattr.to_ne_bytes());
        fixed.extend_from_slice(&0u32.to_ne_bytes());
        if err != 0 {
            fixed.extend_from_slice(&err.to_ne_bytes());
        }
        let ref_pos = 4 + fixed.len();
        let name_ref_placeholder = fixed.len();
        fixed.extend_from_slice(&[0u8; 8]);
        fixed.extend_from_slice(&kind.to_ne_bytes());
        fixed.extend_from_slice(&mtime.0.to_ne_bytes());
        fixed.extend_from_slice(&mtime.1.to_ne_bytes());
        fixed.extend_from_slice(&fileid.to_ne_bytes());
        if let Some(s) = size {
            fixed.extend_from_slice(&s.to_ne_bytes());
        }
        // variable data: name + NUL, then pad whole group to 8
        let name_pos = 4 + fixed.len();
        let off = (name_pos - ref_pos) as i32;
        let nlen = (name.len() + 1) as u32;
        fixed[name_ref_placeholder..name_ref_placeholder + 4].copy_from_slice(&off.to_ne_bytes());
        fixed[name_ref_placeholder + 4..name_ref_placeholder + 8]
            .copy_from_slice(&nlen.to_ne_bytes());
        let mut g = Vec::new();
        let mut body = fixed;
        body.extend_from_slice(name);
        body.push(0);
        while (4 + body.len()) % 8 != 0 {
            body.push(0);
        }
        g.extend_from_slice(&((4 + body.len()) as u32).to_ne_bytes());
        g.extend_from_slice(&body);
        g
    }

    #[test]
    fn parses_documented_layout() {
        let mut buf = group(b"main.rs", VREG, (1_700_000_000, 123), 42, Some(1234), 0);
        buf.extend(group(b"src", VDIR, (5, 6), 43, None, 0));
        buf.extend(group("ファイル".as_bytes(), VLNK, (0, 0), 44, Some(9), 0));
        buf.extend(group(b"broken", 0, (0, 0), 0, None, libc::EACCES as u32));
        let mut got = Vec::new();
        parse_entries(&buf, 4, |e| {
            got.push((e.name.to_vec(), e.kind, e.size, e.mtime, e.fileid, e.error))
        })
        .unwrap();
        assert_eq!(got.len(), 4);
        assert_eq!(got[0].0, b"main.rs");
        assert_eq!(got[0].1, Some(Kind::File));
        assert_eq!(got[0].2, Some(1234));
        assert_eq!(got[0].3, Some(1_700_000_000 * 1_000_000_000 + 123));
        assert_eq!(got[0].4, Some(42));
        assert_eq!(got[1].0, b"src");
        assert_eq!(got[1].1, Some(Kind::Dir));
        assert_eq!(got[1].2, None);
        assert_eq!(got[2].0, "ファイル".as_bytes());
        assert_eq!(got[2].1, Some(Kind::Symlink));
        assert_eq!(got[3].1, Some(Kind::Other));
        assert_eq!(got[3].5, libc::EACCES as u32);
    }

    #[test]
    fn truncated_or_garbage_buffers_fail_closed() {
        let g = group(b"x", VREG, (0, 0), 1, Some(1), 0);
        assert!(parse_entries(&g[..g.len() - 3], 1, |_| {}).is_err());
        assert!(parse_entries(&[0xffu8; 64], 1, |_| {}).is_err());
        assert!(
            parse_entries(&g, 2, |_| {}).is_err(),
            "claims more groups than present"
        );
    }

    #[test]
    fn attrlist_layout_matches_the_abi() {
        const _: () = assert!(size_of::<libc::attrlist>() == 24);
        const _: () = assert!(size_of::<libc::attrreference_t>() == 8);
        const _: () = assert!(size_of::<libc::attribute_set_t>() == RETURNED_LEN);
    }
}
