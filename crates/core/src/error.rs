use std::path::{Path, PathBuf};

/// Every error the engine can produce.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("I/O error: {0}")]
    IoPlain(#[from] std::io::Error),
    #[error("not a directory: {0}")]
    NotADirectory(PathBuf),
    #[error("not supported by this backend: {0}")]
    NotSupported(&'static str),
    #[error("invalid query: {0}")]
    Query(String),
    #[error("index is full: the 32-bit record id space is exhausted")]
    IndexFull,
    #[error("name too long: {0} bytes")]
    NameTooLong(usize),
    #[error("resume token is stale or cannot be honored")]
    TokenStale,
    #[error("index file is corrupt: {0}")]
    Corrupt(String),
    #[error("index file is incompatible: {0}")]
    Incompatible(String),
}

impl Error {
    /// Attach a path to an I/O error.
    pub fn io(path: &Path, source: std::io::Error) -> Self {
        Self::Io {
            path: path.to_path_buf(),
            source,
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;
