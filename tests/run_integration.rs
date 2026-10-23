//! Whole runs: the lock, the state on disk, and the lifecycle callbacks.

mod common;

use fedifetcher::run::{CancelToken, run_once};
use fedifetcher::timeutil;
use httpmock::prelude::*;
use httpmock::Method;

#[tokio::test]
async fn a_run_with_nothing_enabled_still_succeeds() {
    let dir = common::tmp_dir();
    let config = common::test_config(&dir, "example.social");
    let db = fedifetcher::db::open(&config).await.unwrap();

    let message = run_once(config, CancelToken::new(), &db)
        .await
        .ok()
        .expect("a run with nothing enabled should succeed");

    assert!(
        message.starts_with("Processing finished in "),
        "{message}"
    );
    assert!(dir.join("seen_urls").exists(), "state should be written");
    assert!(
        !dir.join("lock.lock").exists(),
        "the lock should be released"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn a_held_lock_stops_the_run() {
    let server = MockServer::start_async().await;
    let dir = common::tmp_dir();
    let fail = server
        .mock_async(|when, then| {
            when.method(Method::GET).path("/fail").query_param_exists("rid");
            then.status(200);
        })
        .await;
    // a lock written just now, held by a live process (this test itself)
    std::fs::write(
        dir.join("lock.lock"),
        serde_json::json!({
            "pid": std::process::id(),
            "created": timeutil::fmt_lock_ts(timeutil::now()),
        })
        .to_string(),
    )
    .unwrap();
    let mut config = common::test_config(&dir, "example.social");
    config.on_fail = Some(format!("{}/fail", server.base_url()));
    let db = fedifetcher::db::open(&config).await.unwrap();

    let err = run_once(config, CancelToken::new(), &db)
        .await
        .err()
        .expect("a held lock must stop the run");

    assert!(err.to_string().contains("Lock file age"), "{err}");
    assert!(
        err.to_string().contains("held by process"),
        "{err}"
    );
    assert!(
        dir.join("lock.lock").exists(),
        "the foreign lock is not ours to remove"
    );
    assert_eq!(fail.hits(), 1, "the failure callback should fire");
}

/// A lock in the old format (no recorded process) is reclaimed, not blocking.
#[tokio::test]
async fn a_legacy_lock_without_a_recorded_process_is_reclaimed() {
    let dir = common::tmp_dir();
    let config = common::test_config(&dir, "example.social");

    std::fs::write(
        dir.join("lock.lock"),
        timeutil::fmt_lock_ts(timeutil::now()),
    )
    .unwrap();
    let db = fedifetcher::db::open(&config).await.unwrap();

    let message = run_once(config, CancelToken::new(), &db)
        .await
        .ok()
        .expect("a lock that names no live process must not block the run");

    assert!(
        message.starts_with("Processing finished in "),
        "{message}"
    );
    assert!(
        !dir.join("lock.lock").exists(),
        "the lock should be released"
    );
}

#[tokio::test]
async fn a_run_fires_the_lifecycle_callbacks() {
    let server = MockServer::start_async().await;
    let dir = common::tmp_dir();
    let start = server
        .mock_async(|when, then| {
            when.method(Method::GET).path("/start").query_param_exists("rid");
            then.status(200);
        })
        .await;
    let done = server
        .mock_async(|when, then| {
            when.method(Method::GET)
                .path("/done")
                .query_param_exists("rid")
                .query_param_exists("ping")
                .query_param_exists("msg");
            then.status(200);
        })
        .await;
    let mut config = common::test_config(&dir, "example.social");
    config.on_start = Some(format!("{}/start", server.base_url()));
    config.on_done = Some(format!("{}/done", server.base_url()));
    let db = fedifetcher::db::open(&config).await.unwrap();

    let message = run_once(config, CancelToken::new(), &db)
        .await
        .ok()
        .expect("the run should succeed");

    assert!(message.starts_with("Processing finished in "), "{message}");
    assert_eq!(start.hits(), 1);
    assert_eq!(done.hits(), 1);
}

/// A lock left behind by a dead process does not block the next run.
#[cfg(unix)]
#[tokio::test]
async fn a_lock_held_by_a_dead_process_is_reclaimed() {
    let dir = common::tmp_dir();
    let config = common::test_config(&dir, "example.social");

    let mut child = std::process::Command::new("sleep")
        .arg("30")
        .spawn()
        .unwrap();
    let dead_pid = child.id();
    child.kill().unwrap();
    child.wait().unwrap();

    std::fs::write(
        dir.join("lock.lock"),
        serde_json::json!({"pid": dead_pid, "created": timeutil::fmt_lock_ts(timeutil::now())})
            .to_string(),
    )
    .unwrap();
    let db = fedifetcher::db::open(&config).await.unwrap();

    let message = run_once(config, CancelToken::new(), &db)
        .await
        .ok()
        .expect("a dead holder's lock must not block the run");

    assert!(
        message.starts_with("Processing finished in "),
        "{message}"
    );
    assert!(
        !dir.join("lock.lock").exists(),
        "the lock should be released"
    );
}

#[tokio::test]
async fn a_cancelled_run_stops_before_doing_anything() {
    let dir = common::tmp_dir();
    // a task is enabled, and the server is unreachable: the cancellation
    // check comes first, so no request is ever made
    let mut config = common::test_config(&dir, "127.0.0.1:9");
    config.home_timeline_length = 10;

    let cancel = CancelToken::new();
    cancel.cancel();
    let db = fedifetcher::db::open(&config).await.unwrap();

    let message = run_once(config, cancel, &db)
        .await
        .ok()
        .expect("a stopped run is not a failure");

    assert_eq!(message, "Stopped.");
    assert!(dir.join("seen_urls").exists(), "state should be written");
    assert!(
        !dir.join("lock.lock").exists(),
        "the lock should be released"
    );
}
