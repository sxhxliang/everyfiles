//! Platform facts injected into the core by the facade.
//!
//! The core contains no `cfg(target_os)`. Anything that differs by platform is
//! a *value* passed in here, so the hot path has one implementation and the
//! core's tests mean the same thing on every CI host.

/// How filename bytes are encoded in the index.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NameEncoding {
    /// Arbitrary bytes, usually UTF-8 (Unix).
    Bytes,
    /// WTF-8, the `OsStr` encoding on Windows; preserves unpaired surrogates.
    Wtf8,
}

/// Per-volume configuration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VolumeConfig {
    /// Path separator byte used when reconstructing paths.
    pub separator: u8,
    /// Whether the *filesystem* distinguishes case. This describes the volume;
    /// the query default is case-insensitive regardless.
    pub case_sensitive_fs: bool,
    pub encoding: NameEncoding,
}

impl VolumeConfig {
    pub const fn unix() -> Self {
        Self {
            separator: b'/',
            case_sensitive_fs: true,
            encoding: NameEncoding::Bytes,
        }
    }

    pub const fn windows() -> Self {
        Self {
            separator: b'\\',
            case_sensitive_fs: false,
            encoding: NameEncoding::Wtf8,
        }
    }
}
