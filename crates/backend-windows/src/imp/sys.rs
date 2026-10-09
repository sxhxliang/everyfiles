//! Raw Win32 plumbing for the directory walk and volume identity.
//!
//! All declarations come from `windows-sys`; this file owns the `unsafe`
//! calls, the RAII handle, and the parsing of the variable-length
//! `FILE_ID_BOTH_DIR_INFO` chain. The chain is walked by `NextEntryOffset`
//! with every step bounds-checked against the buffer the kernel filled, and a
//! zero offset anywhere but the last entry terminates the walk rather than
//! spinning. Fields are read with `read_unaligned`, never by casting to a
//! `&FILE_ID_BOTH_DIR_INFO`, because the kernel only guarantees 8-byte
//! alignment of each entry, not of the struct's natural layout.

use std::ffi::{OsStr, OsString};
use std::io;
use std::mem::offset_of;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::Path;

use windows_sys::Win32::Foundation::{
    ERROR_NO_MORE_FILES, GetLastError, HANDLE, INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_HIDDEN, FILE_ATTRIBUTE_REPARSE_POINT,
    FILE_ATTRIBUTE_SYSTEM, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
    FILE_ID_BOTH_DIR_INFO, FILE_LIST_DIRECTORY, FILE_SHARE_DELETE, FILE_SHARE_READ,
    FILE_SHARE_WRITE, FileIdBothDirectoryInfo, FileIdBothDirectoryRestartInfo,
    GetFileInformationByHandleEx, GetVolumeInformationW, GetVolumePathNameW, OPEN_EXISTING,
};

/// 100-ns intervals between 1601-01-01 and 1970-01-01.
const FILETIME_UNIX_DIFF: i64 = 116_444_736_000_000_000;

/// `FILETIME` as an `i64` → nanoseconds since the Unix epoch.
#[inline]
pub fn filetime_to_unix_ns(ft: i64) -> i64 {
    ft.saturating_sub(FILETIME_UNIX_DIFF).saturating_mul(100)
}

/// NUL-terminated UTF-16 with the `\\?\` extended-length prefix so paths
/// longer than `MAX_PATH` work.
pub fn to_wide_extended(path: &Path) -> Vec<u16> {
    let s = path.as_os_str();
    let bytes = s.as_encoded_bytes();
    let mut w: Vec<u16> = Vec::with_capacity(bytes.len() + 8);
    if bytes.starts_with(br"\\?\") || bytes.starts_with(br"\??\") {
        w.extend(s.encode_wide());
    } else if bytes.starts_with(br"\\") {
        w.extend(OsStr::new(r"\\?\UNC\").encode_wide());
        w.extend(OsStr::new(&path.to_string_lossy()[2..]).encode_wide());
    } else {
        w.extend(OsStr::new(r"\\?\").encode_wide());
        w.extend(s.encode_wide());
    }
    w.push(0);
    w
}

/// Open a directory for listing. Reparse points (junctions, symlinks) are
/// opened as themselves, not followed.
pub fn open_dir(path: &Path) -> io::Result<OwnedHandle> {
    let wide = to_wide_extended(path);
    // SAFETY: `wide` is NUL-terminated and outlives the call; all other
    // arguments are plain values or null where the API allows null.
    let h: HANDLE = unsafe {
        CreateFileW(
            wide.as_ptr(),
            FILE_LIST_DIRECTORY,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
            std::ptr::null_mut(),
        )
    };
    if h == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `h` is a valid handle we own; `OwnedHandle` closes it once.
    Ok(unsafe { OwnedHandle::from_raw_handle(h) })
}

/// One parsed directory entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub name: OsString,
    pub attributes: u32,
    pub size: u64,
    pub mtime_ns: i64,
    pub file_id: u64,
}

impl Entry {
    pub fn is_dir(&self) -> bool {
        self.attributes & FILE_ATTRIBUTE_DIRECTORY != 0
    }
    pub fn is_reparse(&self) -> bool {
        self.attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0
    }
    pub fn is_hidden(&self) -> bool {
        self.attributes & FILE_ATTRIBUTE_HIDDEN != 0
    }
    pub fn is_system(&self) -> bool {
        self.attributes & FILE_ATTRIBUTE_SYSTEM != 0
    }
}

/// Outcome of one `GetFileInformationByHandleEx` call.
pub enum Fetch {
    /// `buf[..n]` holds entries.
    Some(usize),
    Done,
}

/// Fill `buf` with the next batch of `FILE_ID_BOTH_DIR_INFO` entries.
pub fn fetch(h: &OwnedHandle, restart: bool, buf: &mut [u8]) -> io::Result<Fetch> {
    let class = if restart {
        FileIdBothDirectoryRestartInfo
    } else {
        FileIdBothDirectoryInfo
    };
    let len = u32::try_from(buf.len()).unwrap_or(u32::MAX);
    // SAFETY: `h` is a live directory handle; `buf` is writable for `len`
    // bytes, which is the size we pass, and the kernel writes at most that.
    let ok = unsafe {
        GetFileInformationByHandleEx(h.as_raw_handle(), class, buf.as_mut_ptr().cast(), len)
    };
    if ok != 0 {
        // The API does not report bytes written; the chain is self-delimiting
        // via NextEntryOffset, so hand back the whole buffer for parsing.
        return Ok(Fetch::Some(buf.len()));
    }
    // SAFETY: trivially safe FFI call with no arguments.
    let err = unsafe { GetLastError() };
    if err == ERROR_NO_MORE_FILES {
        Ok(Fetch::Done)
    } else {
        Err(io::Error::from_raw_os_error(err as i32))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParseError;

/// Walk the entry chain in `buf`, calling `f` per entry (`.` and `..` are
/// skipped).
pub fn parse_entries(buf: &[u8], mut f: impl FnMut(Entry)) -> Result<(), ParseError> {
    const HDR: usize = size_of::<FILE_ID_BOTH_DIR_INFO>();
    const NAME_OFF: usize = offset_of!(FILE_ID_BOTH_DIR_INFO, FileName);
    let mut pos = 0usize;
    loop {
        if pos + HDR > buf.len() {
            return Err(ParseError);
        }
        // SAFETY: `pos + HDR <= buf.len()` was checked; `read_unaligned`
        // tolerates the 8-byte (not natural) alignment the kernel provides,
        // and every field of the struct is a plain integer so any bit
        // pattern is a valid value.
        let info: FILE_ID_BOTH_DIR_INFO =
            unsafe { std::ptr::read_unaligned(buf.as_ptr().add(pos).cast()) };
        let name_len = info.FileNameLength as usize;
        let name_start = pos + NAME_OFF;
        let name_end = name_start.checked_add(name_len).ok_or(ParseError)?;
        if name_len % 2 != 0 || name_end > buf.len() {
            return Err(ParseError);
        }
        let name_bytes = &buf[name_start..name_end];
        let units: Vec<u16> = name_bytes
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        if units != [b'.' as u16] && units != [b'.' as u16, b'.' as u16] {
            f(Entry {
                name: OsString::from_wide(&units),
                attributes: info.FileAttributes,
                size: u64::try_from(info.EndOfFile).unwrap_or(0),
                mtime_ns: filetime_to_unix_ns(info.LastWriteTime),
                file_id: info.FileId as u64,
            });
        }
        let next = info.NextEntryOffset as usize;
        if next == 0 {
            return Ok(());
        }
        pos = pos.checked_add(next).ok_or(ParseError)?;
    }
}

/// Volume serial number and filesystem name for the volume containing `path`.
pub fn volume_identity(path: &Path) -> io::Result<(u32, OsString)> {
    let wide: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let mut root = [0u16; 512];
    // SAFETY: `wide` is NUL-terminated; `root` is writable for its length.
    let ok = unsafe { GetVolumePathNameW(wide.as_ptr(), root.as_mut_ptr(), root.len() as u32) };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    let mut serial = 0u32;
    let mut max_component = 0u32;
    let mut fs_flags = 0u32;
    let mut fs_name = [0u16; 64];
    // SAFETY: `root` is NUL-terminated (filled by the call above); the out
    // pointers are valid for the call; the name buffer length is correct.
    let ok = unsafe {
        GetVolumeInformationW(
            root.as_ptr(),
            std::ptr::null_mut(),
            0,
            &mut serial,
            &mut max_component,
            &mut fs_flags,
            fs_name.as_mut_ptr(),
            fs_name.len() as u32,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    let n = fs_name
        .iter()
        .position(|&c| c == 0)
        .unwrap_or(fs_name.len());
    Ok((serial, OsString::from_wide(&fs_name[..n])))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filetime_epoch_conversion() {
        assert_eq!(filetime_to_unix_ns(FILETIME_UNIX_DIFF), 0);
        // 2024-02-29T12:34:56Z = 1709210096 s
        let ft = FILETIME_UNIX_DIFF + 1_709_210_096 * 10_000_000;
        assert_eq!(filetime_to_unix_ns(ft), 1_709_210_096 * 1_000_000_000);
    }

    #[test]
    fn extended_prefix_is_applied_once() {
        let w = to_wide_extended(Path::new(r"C:\Users"));
        assert_eq!(String::from_utf16_lossy(&w[..w.len() - 1]), r"\\?\C:\Users");
        let w = to_wide_extended(Path::new(r"\\?\C:\x"));
        assert_eq!(String::from_utf16_lossy(&w[..w.len() - 1]), r"\\?\C:\x");
        let w = to_wide_extended(Path::new(r"\\server\share\d"));
        assert_eq!(
            String::from_utf16_lossy(&w[..w.len() - 1]),
            r"\\?\UNC\server\share\d"
        );
    }

    /// Build a chain the way the kernel lays it out and parse it back.
    #[test]
    fn parses_a_synthetic_chain() {
        fn entry(name: &str, attrs: u32, size: i64, next: bool) -> Vec<u8> {
            let units: Vec<u16> = name.encode_utf16().collect();
            let name_off = offset_of!(FILE_ID_BOTH_DIR_INFO, FileName);
            let mut total = name_off + units.len() * 2;
            total = (total + 7) & !7;
            let mut b = vec![0u8; total];
            let mut info: FILE_ID_BOTH_DIR_INFO = unsafe { std::mem::zeroed() };
            info.NextEntryOffset = if next { total as u32 } else { 0 };
            info.FileAttributes = attrs;
            info.EndOfFile = size;
            info.LastWriteTime = FILETIME_UNIX_DIFF + 10_000_000;
            info.FileNameLength = (units.len() * 2) as u32;
            info.FileId = 77;
            // SAFETY: `b` is at least `size_of::<FILE_ID_BOTH_DIR_INFO>()` bytes.
            unsafe { std::ptr::write_unaligned(b.as_mut_ptr().cast(), info) };
            for (i, u) in units.iter().enumerate() {
                b[name_off + i * 2..name_off + i * 2 + 2].copy_from_slice(&u.to_le_bytes());
            }
            b
        }
        let mut buf = entry(".", FILE_ATTRIBUTE_DIRECTORY, 0, true);
        buf.extend(entry("..", FILE_ATTRIBUTE_DIRECTORY, 0, true));
        buf.extend(entry("Report.docx", 0, 1234, true));
        buf.extend(entry("sub", FILE_ATTRIBUTE_DIRECTORY, 0, true));
        buf.extend(entry("名前.txt", FILE_ATTRIBUTE_HIDDEN, 5, false));
        let mut got = Vec::new();
        parse_entries(&buf, |e| got.push(e)).unwrap();
        assert_eq!(got.len(), 3, "dot entries skipped");
        assert_eq!(got[0].name, "Report.docx");
        assert_eq!(got[0].size, 1234);
        assert_eq!(got[0].mtime_ns, 1_000_000_000);
        assert!(got[1].is_dir());
        assert_eq!(got[2].name, "名前.txt");
        assert!(got[2].is_hidden());
        assert_eq!(got[2].file_id, 77);
    }

    #[test]
    fn garbage_fails_closed() {
        assert!(parse_entries(&[0xffu8; 16], |_| {}).is_err());
        let mut info: FILE_ID_BOTH_DIR_INFO = unsafe { std::mem::zeroed() };
        info.FileNameLength = 10_000;
        let mut b = vec![0u8; size_of::<FILE_ID_BOTH_DIR_INFO>()];
        unsafe { std::ptr::write_unaligned(b.as_mut_ptr().cast(), info) };
        assert!(parse_entries(&b, |_| {}).is_err(), "name past buffer end");
    }
}
