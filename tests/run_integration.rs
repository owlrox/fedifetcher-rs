//! Whole runs: the lock, the state in the database, and the lifecycle callbacks.

mod common;

use fedifetcher::run::{CancelToken, Pause, run_once};
use fedifetcher::timeutil;
use httpmock::Method;
use httpmock::prelude::*;

#[tokio::test]
async fn a_run_with_nothing_enabled_still_succeeds() {
    let dir = common::tmp_dir();
    let config = common::test_config(&dir, "example.social");
    let db = fedifetcher::db::open(&config).await.unwrap();

    let message = run_once(config, CancelToken::new(), Pause::new(), &db)
        .await
        .expect("a run with nothing enabled should succeed");

    assert!(message.starts_with("Processing finished in "), "{message}");
    let tables = fedifetcher::db::list_tables(&db).await.unwrap();
    assert!(
        tables.contains(&"seen_urls".to_string()),
        "state should be written"
    );
    assert!(
        !dir.join("lock.lock").exists(),
        "the lock should be released"
    );
    let runs = fedifetcher::db::recent_runs(&db, 10).await.unwrap();
    assert_eq!(runs.len(), 1, "the run should record itself in the history");
    assert_eq!(runs[0].outcome, "ok");
    assert!(
        runs[0].message.starts_with("Processing finished in "),
        "{}",
        runs[0].message
    );
}

#[cfg(unix)]
#[tokio::test]
async fn a_held_lock_stops_the_run() {
    let server = MockServer::start_async().await;
    let dir = common::tmp_dir();
    let fail = server
        .mock_async(|when, then| {
            when.method(Method::GET)
                .path("/fail")
                .query_param_exists("rid");
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

    let err = run_once(config, CancelToken::new(), Pause::new(), &db)
        .await
        .expect_err("a held lock must stop the run");

    assert!(err.to_string().contains("Lock file age"), "{err}");
    assert!(err.to_string().contains("held by process"), "{err}");
    assert!(
        dir.join("lock.lock").exists(),
        "the foreign lock is not ours to remove"
    );
    assert_eq!(fail.calls(), 1, "the failure callback should fire");
    let runs = fedifetcher::db::recent_runs(&db, 10).await.unwrap();
    assert_eq!(runs.len(), 1, "the refused run should record itself too");
    assert_eq!(runs[0].outcome, "error");
    assert!(
        runs[0].message.contains("Another FediFetcher is running"),
        "{}",
        runs[0].message
    );
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

    let message = run_once(config, CancelToken::new(), Pause::new(), &db)
        .await
        .expect("a lock that names no live process must not block the run");

    assert!(message.starts_with("Processing finished in "), "{message}");
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
            when.method(Method::GET)
                .path("/start")
                .query_param_exists("rid");
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

    let message = run_once(config, CancelToken::new(), Pause::new(), &db)
        .await
        .expect("the run should succeed");

    assert!(message.starts_with("Processing finished in "), "{message}");
    assert_eq!(start.calls(), 1);
    assert_eq!(done.calls(), 1);
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

    let message = run_once(config, CancelToken::new(), Pause::new(), &db)
        .await
        .expect("a dead holder's lock must not block the run");

    assert!(message.starts_with("Processing finished in "), "{message}");
    assert!(
        !dir.join("lock.lock").exists(),
        "the lock should be released"
    );
}

#[tokio::test]
async fn a_canceled_run_stops_before_doing_anything() {
    let dir = common::tmp_dir();
    // a task is enabled, and the server is unreachable: the cancellation
    // check comes first, so no request is ever made
    let mut config = common::test_config(&dir, "127.0.0.1:9");
    config.home_timeline_length = 10;

    let cancel = CancelToken::new();
    cancel.cancel();
    let db = fedifetcher::db::open(&config).await.unwrap();

    let message = run_once(config, cancel, Pause::new(), &db)
        .await
        .expect("a stopped run is not a failure");

    assert!(message.starts_with("Stopped after "), "{message}");
    assert!(message.ends_with('.'), "{message}");
    let tables = fedifetcher::db::list_tables(&db).await.unwrap();
    assert!(
        tables.contains(&"seen_urls".to_string()),
        "state should be written"
    );
    assert!(
        !dir.join("lock.lock").exists(),
        "the lock should be released"
    );
    let runs = fedifetcher::db::recent_runs(&db, 10).await.unwrap();
    assert_eq!(runs.len(), 1, "the stopped run should record itself too");
    assert_eq!(runs[0].outcome, "stopped");
    assert!(
        runs[0].message.starts_with("Stopped after "),
        "{}",
        runs[0].message
    );
}

/// A paused run holds at its checkpoint past the point it would finish,
/// and finishes once it is resumed.
#[tokio::test]
async fn a_paused_run_holds_until_resumed() {
    // the start callback answers slowly, so the run is observably in
    // progress past the moment it is paused
    let server = MockServer::start_async().await;
    server
        .mock_async(|when, then| {
            when.method(Method::GET).path("/start");
            then.status(200).delay(std::time::Duration::from_secs(2));
        })
        .await;
    let dir = common::tmp_dir();
    let mut config = common::test_config(&dir, "example.social");
    config.on_start = Some(format!("{}/start", server.base_url()));
    let db = fedifetcher::db::open(&config).await.unwrap();

    let cancel = CancelToken::new();
    let pause = Pause::new();
    let running = {
        let (config, db) = (config.clone(), db.clone());
        let (cancel, pause) = (cancel.clone(), pause.clone());
        tokio::spawn(async move { run_once(config, cancel, pause, &db).await })
    };

    // pause the run while it is waiting on the start callback
    pause.pause();
    // the callback finishes, and the run would finish with it — but a
    // paused run holds at its next checkpoint instead
    tokio::time::sleep(std::time::Duration::from_secs(3)).await;
    assert!(
        !running.is_finished(),
        "a paused run holds past the point it would finish"
    );

    pause.resume();
    let message = running
        .await
        .unwrap()
        .expect("the resumed run should succeed");
    assert!(message.starts_with("Processing finished in "), "{message}");
}

/// A stop lands while the run is paused, and the run reports it was stopped.
#[tokio::test]
async fn a_stop_lands_while_the_run_is_paused() {
    let server = MockServer::start_async().await;
    server
        .mock_async(|when, then| {
            when.method(Method::GET).path("/start");
            then.status(200).delay(std::time::Duration::from_secs(2));
        })
        .await;
    let dir = common::tmp_dir();
    let mut config = common::test_config(&dir, "example.social");
    config.on_start = Some(format!("{}/start", server.base_url()));
    let db = fedifetcher::db::open(&config).await.unwrap();

    let cancel = CancelToken::new();
    let pause = Pause::new();
    let running = {
        let (config, db) = (config.clone(), db.clone());
        let (cancel, pause) = (cancel.clone(), pause.clone());
        tokio::spawn(async move { run_once(config, cancel, pause, &db).await })
    };

    // pause the run, let it reach its checkpoint (past the callback), then
    // stop it while it is holding
    pause.pause();
    tokio::time::sleep(std::time::Duration::from_secs(3)).await;
    cancel.cancel();

    let message = running
        .await
        .unwrap()
        .expect("a stopped run is not a failure");
    assert!(message.starts_with("Stopped after "), "{message}");
    assert!(message.ends_with('.'), "{message}");
}
