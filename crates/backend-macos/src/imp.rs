//! macOS implementation.

mod fsevents;
mod scanner;
mod sys;

pub use fsevents::FsEventsWatcher;
pub use scanner::MacScanner;

/// Registry name of this backend.
pub const BACKEND_NAME: &str = "macos";
