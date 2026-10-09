//! Linux implementation.

mod fanotify;
mod inotify;
mod scanner;
mod watcher;

pub use fanotify::{FanotifyWatcher, Unavailable};
pub use inotify::InotifyWatcher;
pub use scanner::LinuxScanner;
pub use watcher::LinuxWatcher;

/// Registry name of this backend.
pub const BACKEND_NAME: &str = "linux";
