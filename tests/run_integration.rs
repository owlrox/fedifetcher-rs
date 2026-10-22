//! Whole runs: the lock, the state on disk, and the lifecycle callbacks.

mod common;

use fedifetcher::run::run_once;
use fedifetcher::timeutil;
use httpmock::prelude::*;
use httpmock::Method;

#[tokio::test]
async fn a_run_with_nothing_enabled_still_succeeds() {
    let dir = common::tmp_dir();
    let config = common::test_config(&dir, "example.social");

    let message = run_once(config)
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
    // a lock written just now, as if another run held it
    std::fs::write(
        dir.join("lock.lock"),
        timeutil::fmt_lock_ts(timeutil::now()),
    )
    .unwrap();
    let mut config = common::test_config(&dir, "example.social");
    config.on_fail = Some(format!("{}/fail", server.base_url()));

    let err = run_once(config)
        .await
        .err()
        .expect("a held lock must stop the run");

    assert!(err.to_string().contains("Lock file age"), "{err}");
    assert!(
        dir.join("lock.lock").exists(),
        "the foreign lock is not ours to remove"
    );
    assert_eq!(fail.hits(), 1, "the failure callback should fire");
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

    let message = run_once(config)
        .await
        .ok()
        .expect("the run should succeed");

    assert!(message.starts_with("Processing finished in "), "{message}");
    assert_eq!(start.hits(), 1);
    assert_eq!(done.hits(), 1);
}
