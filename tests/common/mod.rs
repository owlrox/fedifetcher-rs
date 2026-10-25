//! Shared helpers for the integration tests.

use std::path::{Path, PathBuf};

use fedifetcher::config::Config;

/// A fresh temporary directory, unique to this test.
pub fn tmp_dir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "fedifetcher-test-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A config that is valid except for pointing at the given server, with the
/// state kept in the given directory.
pub fn test_config(dir: &Path, server: &str) -> Config {
    Config {
        server: server.to_string(),
        access_tokens: vec!["token".to_string()],
        state_dir: dir.to_path_buf(),
        ..Default::default()
    }
}
