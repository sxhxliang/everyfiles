//! macOS backend: `getattrlistbulk(2)` bulk enumeration and FSEvents watching.
//!
//! Everything platform-specific lives behind `cfg(target_os = "macos")` so the
//! crate always exists as a workspace member and compiles to an empty library
//! elsewhere. That is what lets the workspace build on every host while
//! `cargo xtask xcheck` type-checks each backend for its own target.

#[cfg(target_os = "macos")]
mod imp;

#[cfg(target_os = "macos")]
pub use imp::*;
