//! Error types.
//!
//! FediFetcher's errors carry the message that is logged. Each layer has its
//! own type so callers can tell a configuration problem from a lock conflict
//! from a general failure, and `From` impls let `?` convert between them.

/// A general failure: an HTTP error, a database error, a parse failure.
/// The message is what gets logged.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct Error(pub String);

impl From<String> for Error {
    fn from(message: String) -> Self {
        Error(message)
    }
}

impl From<&str> for Error {
    fn from(message: &str) -> Self {
        Error(message.to_string())
    }
}

/// A configuration problem: an unknown log level, a value that will not
/// coerce, a missing required option.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct ConfigError(pub String);

impl From<ConfigError> for Error {
    fn from(err: ConfigError) -> Self {
        Error(err.0)
    }
}

/// Another run holds the lock, or the lock file cannot be read.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct LockedError(pub String);

impl From<LockedError> for Error {
    fn from(err: LockedError) -> Self {
        Error(err.0)
    }
}

pub type Result<T> = std::result::Result<T, Error>;
