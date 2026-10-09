//! Linux backend: parallel `getdents64` enumeration, fanotify (root) or
//! inotify (unprivileged) watching.
//!
//! Platform code is gated on `cfg(target_os = "linux")`; see the macOS crate
//! for why.

#[cfg(target_os = "linux")]
mod imp;

#[cfg(target_os = "linux")]
pub use imp::*;
