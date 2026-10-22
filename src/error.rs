//! Error types. The Python original raises plain `Exception`s with messages
//! that end up in the log; here they are strings wrapped in a newtype so the
//! messages read the same.

use std::fmt;

/// The equivalent of a Python `Exception`: a message that is logged where the
/// Python code logged the exception.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error(pub String);

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

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

/// Raised when the supplied configuration cannot be used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigError(pub String);

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ConfigError {}

impl From<ConfigError> for Error {
    fn from(err: ConfigError) -> Self {
        Error(err.0)
    }
}

/// Raised when another run holds the lock, or its age cannot be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LockedError(pub String);

impl fmt::Display for LockedError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for LockedError {}

impl From<LockedError> for Error {
    fn from(err: LockedError) -> Self {
        Error(err.0)
    }
}

pub type Result<T> = std::result::Result<T, Error>;
