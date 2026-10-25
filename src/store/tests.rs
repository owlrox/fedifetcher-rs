//! The tests for `store.rs`. Kept in a child module so they see the
//! private items; the parent holds only the `mod tests;` pointer.

use super::*;
use serde_json::json;

fn test_config(dir: &std::path::Path) -> Config {
    Config {
        server: "home.example".to_string(),
        access_tokens: vec!["token".to_string()],
        state_dir: dir.to_path_buf(),
        ..Default::default()
    }
}

fn temp_dir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("ff-store-test-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[tokio::test]
async fn state_round_trips_through_the_database() {
    let dir = temp_dir();
    let config = test_config(&dir);
    let pool = db::open(&config).await.unwrap();
    let store = StateStore::new(config);

    let mut session = store.session(&pool).await.unwrap();
    let state = session.state();
    state
        .seen_urls_mut()
        .add("https://remote.example/@a/1", None);
    state.known_followings_mut().add("b@remote.example", None);
    state
        .recently_checked_users_mut()
        .add("c@remote.example", None);
    state.replied_toot_server_ids_mut().insert(
        "https://home.example/@b/2".to_string(),
        Some(RepliedToot {
            url: "https://remote.example/@a/1".to_string(),
            post_ref: PostRef::new("remote.example", "1"),
        }),
    );
    state
        .replied_toot_server_ids_mut()
        .insert("https://home.example/@b/3".to_string(), None);
    state.seen_hosts_mut().add(
        "remote.example",
        json!({
            "webserver": "remote.example", "software": "mastodon", "version": "4.3",
            "last_checked": timeutil::fmt_state_ts(timeutil::now()),
            "mastodonApiSupport": true, "peertubeApiSupport": false,
        }),
    );
    state
        .recently_checked_context_mut()
        .mark_fetched("uri", timeutil::now());
    session.finish().await.unwrap();

    let loaded = store.load(&pool).await.unwrap();
    assert!(loaded.seen_urls().contains("https://remote.example/@a/1"));
    assert!(loaded.known_followings().contains("b@remote.example"));
    assert!(loaded.recently_checked_users().contains("c@remote.example"));
    assert!(loaded.seen_hosts().contains("remote.example"));
    assert!(loaded.recently_checked_context().contains("uri"));
    assert_eq!(
        loaded
            .replied_toot_server_ids()
            .get("https://home.example/@b/2"),
        Some(&Some(RepliedToot {
            url: "https://remote.example/@a/1".to_string(),
            post_ref: PostRef::new("remote.example", "1")
        }))
    );
    assert_eq!(
        loaded
            .replied_toot_server_ids()
            .get("https://home.example/@b/3"),
        Some(&None)
    );
    // all_known_users is rebuilt from followings and recently checked users
    assert!(loaded.all_known_users().contains("b@remote.example"));
    assert!(loaded.all_known_users().contains("c@remote.example"));

    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn dropping_a_session_without_finishing_loses_the_changes() {
    let dir = temp_dir();
    let config = test_config(&dir);
    let pool = db::open(&config).await.unwrap();
    let store = StateStore::new(config);
    {
        let mut session = store.session(&pool).await.unwrap();
        session
            .state()
            .seen_urls_mut()
            .add("https://x.example/1", None);
        // dropped without finish: the run's changes are lost
    }
    let loaded = store.load(&pool).await.unwrap();
    assert!(loaded.seen_urls().is_empty());
    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn load_applies_the_configured_expiry() {
    let dir = temp_dir();
    let mut config = test_config(&dir);
    config.remember_users_for_hours = 24;
    let pool = db::open(&config).await.unwrap();
    let old = timeutil::fmt_state_ts(timeutil::now() - Duration::days(10));
    let fresh = timeutil::fmt_state_ts(timeutil::now());
    for (acct, first_seen) in [
        ("old@remote.example", &old),
        ("fresh@remote.example", &fresh),
    ] {
        sqlx::query("INSERT INTO recently_checked_users (acct, first_seen) VALUES (?, ?)")
            .bind(acct)
            .bind(first_seen)
            .execute(&pool)
            .await
            .unwrap();
    }

    let store = StateStore::new(config);
    let loaded = store.load(&pool).await.unwrap();
    assert!(
        !loaded
            .recently_checked_users()
            .contains("old@remote.example"),
        "a user remembered longer ago than remember_users_for_hours is forgotten"
    );
    assert!(
        loaded
            .recently_checked_users()
            .contains("fresh@remote.example")
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn lock_blocks_a_second_run_until_it_expires() {
    let dir = temp_dir();
    let config = test_config(&dir);

    let guard = lock(&config).unwrap();
    assert!(config.lock_path().exists());
    let err = lock(&config).unwrap_err();
    assert!(err.to_string().contains("below --lock-hours=24"), "{err}");
    drop(guard);
    assert!(!config.lock_path().exists());

    // an expired lock is removed and re-acquired
    std::fs::write(
        config.lock_path(),
        timeutil::fmt_lock_ts(timeutil::now() - Duration::hours(48)),
    )
    .unwrap();
    let guard = lock(&config).unwrap();
    drop(guard);
    assert!(!config.lock_path().exists());

    // an unreadable lock aborts
    std::fs::write(config.lock_path(), "not a timestamp").unwrap();
    let err = lock(&config).unwrap_err();
    assert!(
        err.to_string().starts_with("Cannot read lock file"),
        "{err}"
    );

    std::fs::remove_dir_all(&dir).ok();
}

/// A lock in the old format — a bare timestamp, no recorded process —
/// cannot name a holder, so it is reclaimed rather than blocking.
#[test]
fn a_legacy_lock_without_a_recorded_process_is_reclaimed() {
    let dir = temp_dir();
    let config = test_config(&dir);

    std::fs::write(config.lock_path(), timeutil::fmt_lock_ts(timeutil::now())).unwrap();
    let guard = lock(&config).expect("a legacy lock is reclaimed");
    assert!(config.lock_path().exists());
    drop(guard);
    assert!(!config.lock_path().exists());

    std::fs::remove_dir_all(&dir).ok();
}

/// A lock held by a process that no longer exists is reclaimed.
#[cfg(unix)]
#[test]
fn a_lock_held_by_a_dead_process_is_reclaimed() {
    let dir = temp_dir();
    let config = test_config(&dir);

    // a process that is dead by the time the lock is read
    let mut child = std::process::Command::new("sleep")
        .arg("30")
        .spawn()
        .unwrap();
    let dead_pid = child.id();
    child.kill().unwrap();
    child.wait().unwrap();
    assert!(!process_alive(dead_pid));

    std::fs::write(
        config.lock_path(),
        serde_json::json!({"pid": dead_pid, "created": timeutil::fmt_lock_ts(timeutil::now())})
            .to_string(),
    )
    .unwrap();
    let guard = lock(&config).expect("a dead holder's lock is reclaimed");
    assert!(config.lock_path().exists());
    drop(guard);

    std::fs::remove_dir_all(&dir).ok();
}

/// A lock held by a live process blocks, and says who holds it.
#[cfg(unix)]
#[test]
fn a_lock_held_by_a_live_process_blocks_and_names_it() {
    let dir = temp_dir();
    let config = test_config(&dir);

    let live_pid = std::process::id();
    std::fs::write(
        config.lock_path(),
        serde_json::json!({"pid": live_pid, "created": timeutil::fmt_lock_ts(timeutil::now())})
            .to_string(),
    )
    .unwrap();
    let err = lock(&config).unwrap_err();
    assert!(err.to_string().contains("below --lock-hours=24"), "{err}");
    assert!(
        err.to_string()
            .contains(&format!("held by process {live_pid}")),
        "{err}"
    );
    assert!(
        config.lock_path().exists(),
        "the live holder's lock is not removed"
    );

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn lock_content_reads_both_formats() {
    let legacy = parse_lock_content(&timeutil::fmt_lock_ts(timeutil::now())).unwrap();
    assert_eq!(legacy.pid, None);

    let current = parse_lock_content(
        &serde_json::json!({"pid": 123, "created": timeutil::fmt_lock_ts(timeutil::now())})
            .to_string(),
    )
    .unwrap();
    assert_eq!(current.pid, Some(123));

    assert!(parse_lock_content("not a timestamp").is_none());
}

#[tokio::test]
async fn finishing_a_session_whose_state_was_never_taken_writes_anyway() {
    let dir = temp_dir();
    let config = test_config(&dir);
    let pool = db::open(&config).await.unwrap();
    let store = StateStore::new(config);

    let session = store.session(&pool).await.unwrap();
    session.finish().await.unwrap();

    // and the database still reads back cleanly
    let loaded = store.load(&pool).await.unwrap();
    assert!(loaded.seen_urls().is_empty());
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_lock_whose_holder_is_dead_is_reclaimed_at_once() {
    let dir = temp_dir();
    let config = test_config(&dir);
    // a recent lock naming a process id that cannot exist
    std::fs::write(
        config.lock_path(),
        serde_json::json!({
            "pid": 999_999_999,
            "created": timeutil::fmt_lock_ts(timeutil::now()),
        })
        .to_string(),
    )
    .unwrap();

    let guard_lock = lock(&config).unwrap();

    // the reclaim rewrote the lock file with a live holder's record
    let reclaimed =
        parse_lock_content(&std::fs::read_to_string(config.lock_path()).unwrap()).unwrap();
    assert_eq!(
        reclaimed.pid,
        Some(std::process::id()),
        "the dead holder's pid is replaced by ours"
    );
    drop(guard_lock);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_lock_from_a_version_that_recorded_no_process_is_reclaimed() {
    let dir = temp_dir();
    let config = test_config(&dir);
    // the bare-timestamp shape older versions wrote
    std::fs::write(config.lock_path(), timeutil::fmt_lock_ts(timeutil::now())).unwrap();

    let guard_lock = lock(&config).unwrap();

    // the reclaim rewrote the lock file, recording a holder this time
    let reclaimed =
        parse_lock_content(&std::fs::read_to_string(config.lock_path()).unwrap()).unwrap();
    assert_eq!(reclaimed.pid, Some(std::process::id()));
    drop(guard_lock);
    std::fs::remove_dir_all(&dir).ok();
}
