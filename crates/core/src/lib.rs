//! everyfiles-core: a portable in-memory filename index.
//!
//! The engine holds every filename on a volume in a struct-of-arrays index and
//! answers substring queries in milliseconds. It is deliberately free of any
//! platform dependency and any `cfg(target_os)`: everything platform-specific
//! arrives through the [`scan::Scanner`] trait implemented by a backend crate,
//! and through the injected [`platform::VolumeConfig`].
//!
//! Module map:
//! - [`index`] — the record arrays and the two name pools.
//! - [`scan`] — the backend seam and the parallel tree assembler.
//! - [`query`] — matching, filtering, sorting, path reconstruction.
//! - [`fold`] — case folding shared by ingest and query.
//! - [`store`] — the on-disk format, zero-copy load, atomic save.
//! - [`engine`] — open an index, apply incremental updates, drive a watcher.
//! - [`watch`] — the change-notification seam.

#![forbid(unsafe_op_in_unsafe_fn)]
#![warn(missing_debug_implementations)]

pub mod engine;
pub mod error;
pub mod fold;
pub mod index;
pub mod platform;
pub mod query;
pub mod scan;
pub mod store;
#[cfg(any(test, feature = "testing"))]
pub mod testing;
pub mod watch;

pub use arc_swap;
pub use error::{Error, Result};
pub use index::{Id, Index, ROOT};
