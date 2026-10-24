//! The log pipeline: a configured level is accepted or refused, lines reach
//! the database the /logs page reads, and shutdown loses nothing.

mod common;

use fedifetcher::config::Config;
use fedifetcher::db::{self, LogOrder, LogSort};
use fedifetcher::logging;

/// An unknown level is refused before anything is set up.
#[tokio::test]
async fn an_unknown_log_level_is_refused() {
    let dir = common::tmp_dir();
    let mut config = common::test_config(&dir, "example.social");
    config.log_level = "LOUD".to_string();
    let db = db::open(&config).await.unwrap();

    let err = logging::setup(&config, db)
        .err()
        .expect("an unknown level must be refused");
    let err = err.to_string();
    assert!(err.contains("Unknown level"), "{err}");
    assert!(err.contains("LOUD"), "{err}");
}

/// Lines logged while the writer runs reach the database with their level
/// spelled the way the config names them, and shutdown has written them
/// all by the time it returns.
#[tokio::test]
async fn logged_lines_reach_the_database_and_shutdown_writes_them_out() {
    let dir = common::tmp_dir();
    let config = common::test_config(&dir, "example.social");
    let db = db::open(&config).await.unwrap();

    let log_db = logging::setup(&config, db.clone()).expect("DEBUG is a known level");

    tracing::info!("hello from the logging test");
    tracing::warn!("careful from the logging test");

    log_db.shutdown().await;

    let rows = db::recent_logs(&db, 100, LogSort::Time, LogOrder::Desc, None)
        .await
        .unwrap();
    let info = rows
        .iter()
        .find(|row| row.message.contains("hello from the logging test"))
        .expect("the info line is in the database");
    assert_eq!(info.level, "INFO");
    assert!(!info.ts.is_empty());
    let warn = rows
        .iter()
        .find(|row| row.message.contains("careful from the logging test"))
        .expect("the warning line is in the database");
    assert_eq!(warn.level, "WARNING", "WARN is spelled the config's way");
    assert_eq!(info.target, "logging_integration");
}

/// The configured format renders the way a stored row does.
#[test]
fn the_stored_line_matches_the_configured_format() {
    let config = Config::default();
    let line = logging::format_db_line(
        &config.log_format,
        "INFO",
        "2026-10-24 07:00:00 UTC",
        "Found 2 reply toots",
    );
    assert_eq!(line, "2026-10-24 07:00:00 UTC: Found 2 reply toots");
}
