//! One error type for the whole crate: what failed is a variant, and the
//! `Display` text is the message that gets logged. `From` impls let `?`
//! convert from the underlying error types (`sqlx`, `serde_json`,
//! `roxmltree`), and the variants with a `#[source]` keep the cause for
//! `anyhow`-style inspection and testing.

use std::path::PathBuf;

/// What went wrong.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A database query or transaction failed.
    #[error("Database error: {0}")]
    Db(#[from] sqlx::Error),
    /// The database file could not be opened.
    #[error("Could not open {path}: {source}")]
    DbOpen {
        path: PathBuf,
        #[source]
        source: sqlx::Error,
    },
    /// A file could not be read or written.
    #[error("{context}: {source}")]
    Io {
        context: String,
        #[source]
        source: std::io::Error,
    },
    /// A request could not be sent: DNS, a refused connection, TLS, a
    /// timeout.
    #[error("Error requesting {url}: {}", error_chain(source))]
    Http {
        url: String,
        #[source]
        source: reqwest::Error,
    },
    /// A response's body could not be read.
    #[error("Error reading response from {url}: {}", error_chain(source))]
    HttpRead {
        url: String,
        #[source]
        source: reqwest::Error,
    },
    /// A response body was not the JSON it was expected to be.
    #[error("Error parsing response as JSON: {0}")]
    Json(#[from] serde_json::Error),
    /// An XML document (host-meta) did not parse.
    #[error("{0}")]
    Xml(#[from] roxmltree::Error),
    /// A configuration value is missing or will not coerce.
    #[error("{0}")]
    Config(String),
    /// Another run holds the lock, or the lock file cannot be read.
    #[error("{0}")]
    Locked(String),
    /// A fetch failed: the server's answer was missing or unexpected, or
    /// the request was not allowed.
    #[error("{0}")]
    Fetch(String),
}

/// The crate's result type.
pub type Result<T> = std::result::Result<T, Error>;

/// An error with its whole source chain spelled out, so a failed request
/// says why it failed — a DNS error, a refused connection, a TLS error, a
/// timeout — rather than reqwest's "error sending request for url (...)"
/// alone, which hides the cause.
fn error_chain(error: &impl std::error::Error) -> String {
    let mut text = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        text.push_str(": ");
        text.push_str(&cause.to_string());
        source = cause.source();
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stand-in for reqwest's error chain: an outer error, a middle
    /// cause, and the root cause at the bottom.
    #[derive(Debug)]
    struct ChainError(&'static str, Option<Box<ChainError>>);

    impl std::fmt::Display for ChainError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str(self.0)
        }
    }

    impl std::error::Error for ChainError {
        fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
            self.1
                .as_deref()
                .map(|cause| cause as &(dyn std::error::Error + 'static))
        }
    }

    #[test]
    fn error_chain_spells_out_every_source() {
        let error = ChainError(
            "error sending request for url (https://example.invalid/)",
            Some(Box::new(ChainError(
                "client error (Connect)",
                Some(Box::new(ChainError(
                    "dns error: failed to lookup address information",
                    None,
                ))),
            ))),
        );
        assert_eq!(
            error_chain(&error),
            "error sending request for url (https://example.invalid/): client error (Connect): dns error: failed to lookup address information"
        );
        // an error without sources is just itself
        assert_eq!(error_chain(&ChainError("just this", None)), "just this");
    }

    #[test]
    fn a_database_error_converts_with_its_message() {
        let error = Error::from(sqlx::Error::RowNotFound);
        assert!(error
            .to_string()
            .starts_with("Database error: no rows returned"));
        assert!(matches!(error, Error::Db(_)));
    }

    #[test]
    fn a_json_error_converts_with_its_message() {
        let json = serde_json::from_str::<serde_json::Value>("not json").unwrap_err();
        let error = Error::from(json);
        assert!(error
            .to_string()
            .starts_with("Error parsing response as JSON: "));
        assert!(matches!(error, Error::Json(_)));
    }
}
