//! Windows backend: `FileIdBothDirectoryInfo` bulk walk (unprivileged) and the
//! NTFS USN journal (elevated) for enumeration and resumable watching.
//!
//! Platform code is gated on `cfg(target_os = "windows")`; see the macOS crate
//! for why.

#[cfg(target_os = "windows")]
mod imp;

#[cfg(target_os = "windows")]
pub use imp::*;
