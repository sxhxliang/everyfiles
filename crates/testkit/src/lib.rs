//! Test support shared by the core, the backends, and the CLI.
//!
//! - [`SyntheticScanner`]: an in-memory [`Scanner`] so the entire core can be
//!   tested with no filesystem at all.
//! - [`oracle::naive_scan`]: the `walkdir`-based reference the differential
//!   test and `everyfiles verify` both compare against.
//! - [`fixture::create_tree`]: builds a real tree in a directory from a spec.

pub mod fixture;
pub mod oracle;
pub mod synthetic;

pub use synthetic::{GeneratedScanner, SyntheticScanner};
