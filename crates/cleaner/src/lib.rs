//! `everyfiles-cleaner` — CleanMyMac-style system cleaner and developer suite
//! built on everyfiles' high-speed filesystem engine.

#![allow(clippy::too_many_arguments, clippy::collapsible_if)]

pub mod deleter;
pub mod developer;
pub mod domain;
pub mod duplicates;
pub mod guard;
pub mod maintenance;
pub mod scanners;
pub mod space_lens;
pub mod tui;
pub mod uninstaller;

pub use deleter::SafeDeleter;
pub use developer::{ArtifactKind, DeveloperManager, GitRepoInfo, ProjectArtifact};
pub use domain::{
    CleanCategory, CleanGroup, CleanItem, CleanReport, SafetyLevel, ScanSummary, SubItem,
    format_size,
};
pub use duplicates::{DuplicateFile, DuplicateFinder, DuplicateGroup, DuplicateReport};
pub use guard::SafetyGuard;
pub use maintenance::SystemMaintenance;
pub use scanners::CleanupCoordinator;
pub use space_lens::{SpaceLens, SpaceNode};
pub use tui::CleanerApp;
pub use uninstaller::{AppAssociatedFile, AppUninstaller, InstalledApp};
