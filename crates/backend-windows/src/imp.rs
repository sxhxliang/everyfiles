//! Windows implementation.

mod auto;
mod scanner;
mod sys;
mod usn;
mod watcher;

pub use auto::WindowsWatcher;
pub use scanner::WindowsScanner;
pub use usn::{Unavailable, UsnWatcher};
pub use watcher::RdcwWatcher;

/// Registry name of this backend.
pub const BACKEND_NAME: &str = "windows";
