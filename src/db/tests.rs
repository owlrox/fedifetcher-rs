//! The tests for `db.rs`. Kept in a child module so they see the
//! private items; the parent holds only the `mod tests;` pointer.

use super::*;

fn test_config() -> (Config, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!(
        "fedifetcher-db-test-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    let config = Config {
        state_dir: dir.clone(),
        ..Default::default()
    };
    (config, dir)
}

fn log_row(message: &str) -> LogRow {
    LogRow {
        ts: "2026-10-22 16:05:07 -05:00".to_string(),
        level: "INFO".to_string(),
        target: "fedifetcher::test".to_string(),
        message: message.to_string(),
    }
}

#[tokio::test]
async fn log_lines_round_trip_newest_first() {
    let (config, dir) = test_config();
    let pool = open(&config).await.unwrap();
    assert!(dir.join("fedifetcher.db").exists(), "the file is created");

    insert_log(&pool, &log_row("first")).await.unwrap();
    insert_log(&pool, &log_row("second")).await.unwrap();
    insert_log(&pool, &log_row("third")).await.unwrap();

    let lines = recent_logs(&pool, 10, LogSort::Time, LogOrder::Desc, None)
        .await
        .unwrap();
    let messages: Vec<&str> = lines.iter().map(|row| row.message.as_str()).collect();
    assert_eq!(messages, ["third", "second", "first"]);
    assert_eq!(lines[0].level, "INFO");
    assert_eq!(lines[0].target, "fedifetcher::test");
}

#[tokio::test]
async fn time_ascending_is_oldest_first() {
    let (config, _dir) = test_config();
    let pool = open(&config).await.unwrap();
    for message in ["first", "second", "third"] {
        insert_log(&pool, &log_row(message)).await.unwrap();
    }
    let lines = recent_logs(&pool, 10, LogSort::Time, LogOrder::Asc, None)
        .await
        .unwrap();
    let messages: Vec<&str> = lines.iter().map(|row| row.message.as_str()).collect();
    assert_eq!(messages, ["first", "second", "third"]);
}

#[tokio::test]
async fn recent_logs_returns_only_the_last_lines() {
    let (config, _dir) = test_config();
    let pool = open(&config).await.unwrap();
    for n in 0..5 {
        insert_log(&pool, &log_row(&format!("line {n}")))
            .await
            .unwrap();
    }
    let lines = recent_logs(&pool, 2, LogSort::Time, LogOrder::Desc, None)
        .await
        .unwrap();
    let messages: Vec<&str> = lines.iter().map(|row| row.message.as_str()).collect();
    assert_eq!(messages, ["line 4", "line 3"]);
}

#[tokio::test]
async fn sorting_by_level_orders_alphabetically() {
    let (config, _dir) = test_config();
    let pool = open(&config).await.unwrap();
    for (level, message) in [("ERROR", "c"), ("INFO", "a"), ("DEBUG", "b")] {
        let mut row = log_row(message);
        row.level = level.to_string();
        insert_log(&pool, &row).await.unwrap();
    }
    let ascending = recent_logs(&pool, 10, LogSort::Level, LogOrder::Asc, None)
        .await
        .unwrap();
    let messages: Vec<&str> = ascending.iter().map(|row| row.message.as_str()).collect();
    assert_eq!(messages, ["b", "c", "a"], "DEBUG, ERROR, INFO");
    let descending = recent_logs(&pool, 10, LogSort::Level, LogOrder::Desc, None)
        .await
        .unwrap();
    let messages: Vec<&str> = descending.iter().map(|row| row.message.as_str()).collect();
    assert_eq!(messages, ["a", "c", "b"], "INFO, ERROR, DEBUG");
}

#[tokio::test]
async fn sorting_by_message_orders_alphabetically() {
    let (config, _dir) = test_config();
    let pool = open(&config).await.unwrap();
    for message in ["zebra", "apple", "mango"] {
        insert_log(&pool, &log_row(message)).await.unwrap();
    }
    let lines = recent_logs(&pool, 10, LogSort::Message, LogOrder::Asc, None)
        .await
        .unwrap();
    let messages: Vec<&str> = lines.iter().map(|row| row.message.as_str()).collect();
    assert_eq!(messages, ["apple", "mango", "zebra"]);
}

/// Insert one log line per level, oldest first.
async fn insert_lines_per_level(pool: &SqlitePool) {
    for (level, message) in [
        ("DEBUG", "a debug line"),
        ("INFO", "an info line"),
        ("WARNING", "a warning line"),
        ("ERROR", "an error line"),
    ] {
        let mut row = log_row(message);
        row.level = level.to_string();
        insert_log(pool, &row).await.unwrap();
    }
}

fn levels(names: &[&str]) -> Vec<String> {
    names.iter().map(|name| name.to_string()).collect()
}

#[tokio::test]
async fn recent_logs_filters_by_level() {
    let (config, _dir) = test_config();
    let pool = open(&config).await.unwrap();
    insert_lines_per_level(&pool).await;

    let lines = recent_logs(
        &pool,
        10,
        LogSort::Time,
        LogOrder::Desc,
        Some(&levels(&["WARNING", "ERROR"])),
    )
    .await
    .unwrap();
    let messages: Vec<&str> = lines.iter().map(|row| row.message.as_str()).collect();
    assert_eq!(messages, ["an error line", "a warning line"]);

    // no level selected: nothing
    let lines = recent_logs(&pool, 10, LogSort::Time, LogOrder::Desc, Some(&[]))
        .await
        .unwrap();
    assert!(lines.is_empty());

    // no filter: every level, newest first
    let lines = recent_logs(&pool, 10, LogSort::Time, LogOrder::Desc, None)
        .await
        .unwrap();
    let messages: Vec<&str> = lines.iter().map(|row| row.message.as_str()).collect();
    assert_eq!(
        messages,
        [
            "an error line",
            "a warning line",
            "an info line",
            "a debug line"
        ]
    );
}

#[tokio::test]
async fn the_level_filter_applies_before_the_line_limit() {
    let (config, _dir) = test_config();
    let pool = open(&config).await.unwrap();
    // the ERROR lines are the oldest; a limit applied before the
    // filter would never reach them
    for message in ["the first error line", "the second error line"] {
        let mut row = log_row(message);
        row.level = "ERROR".to_string();
        insert_log(&pool, &row).await.unwrap();
    }
    for n in 0..3 {
        insert_log(&pool, &log_row(&format!("info line {n}")))
            .await
            .unwrap();
    }

    let lines = recent_logs(
        &pool,
        2,
        LogSort::Time,
        LogOrder::Desc,
        Some(&levels(&["ERROR"])),
    )
    .await
    .unwrap();
    let messages: Vec<&str> = lines.iter().map(|row| row.message.as_str()).collect();
    assert_eq!(messages, ["the second error line", "the first error line"]);
}

#[tokio::test]
async fn clearing_removes_every_line() {
    let (config, _dir) = test_config();
    let pool = open(&config).await.unwrap();
    for n in 0..3 {
        insert_log(&pool, &log_row(&format!("line {n}")))
            .await
            .unwrap();
    }
    clear_logs(&pool).await.unwrap();
    assert!(
        recent_logs(&pool, 10, LogSort::Time, LogOrder::Desc, None)
            .await
            .unwrap()
            .is_empty()
    );
    // clearing an empty log is fine, and new lines still arrive
    clear_logs(&pool).await.unwrap();
    insert_log(&pool, &log_row("after clearing")).await.unwrap();
    let lines = recent_logs(&pool, 10, LogSort::Time, LogOrder::Desc, None)
        .await
        .unwrap();
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].message, "after clearing");
}

#[tokio::test]
async fn pruning_keeps_the_newest_lines() {
    let (config, _dir) = test_config();
    let pool = open(&config).await.unwrap();
    for n in 0..3 {
        insert_log(&pool, &log_row(&format!("line {n}")))
            .await
            .unwrap();
    }
    prune_logs(&pool, 1).await.unwrap();
    let lines = recent_logs(&pool, 10, LogSort::Time, LogOrder::Desc, None)
        .await
        .unwrap();
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].message, "line 2");
}

#[tokio::test]
async fn pruning_an_empty_log_changes_nothing() {
    let (config, _dir) = test_config();
    let pool = open(&config).await.unwrap();
    prune_logs(&pool, 100).await.unwrap();
    assert!(
        recent_logs(&pool, 10, LogSort::Time, LogOrder::Desc, None)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn runs_round_trip_newest_first() {
    let (config, _dir) = test_config();
    let pool = open(&config).await.unwrap();

    let run = |outcome: &str| RunRow {
        started: "2026-10-22 16:05:07.000001-05:00".to_string(),
        finished: "2026-10-22 16:05:08.000001-05:00".to_string(),
        outcome: outcome.to_string(),
        message: "Processing finished in 0:00:01".to_string(),
    };
    insert_run(&pool, &run("ok")).await.unwrap();
    insert_run(&pool, &run("error")).await.unwrap();

    let runs = recent_runs(&pool, 10).await.unwrap();
    assert_eq!(runs.len(), 2);
    assert_eq!(runs[0].outcome, "error", "the newest run comes first");
    assert_eq!(runs[1].outcome, "ok");
    assert_eq!(runs[0].started, "2026-10-22 16:05:07.000001-05:00");
}

#[tokio::test]
async fn recent_runs_returns_only_the_last_runs() {
    let (config, _dir) = test_config();
    let pool = open(&config).await.unwrap();
    for n in 0..3 {
        insert_run(
            &pool,
            &RunRow {
                started: format!("start {n}"),
                finished: format!("finish {n}"),
                outcome: "ok".to_string(),
                message: String::new(),
            },
        )
        .await
        .unwrap();
    }
    let runs = recent_runs(&pool, 2).await.unwrap();
    assert_eq!(runs[0].started, "start 2");
    assert_eq!(runs[1].started, "start 1");
}

#[tokio::test]
async fn a_run_is_recorded_while_running_and_completed_when_done() {
    let (config, _dir) = test_config();
    let pool = open(&config).await.unwrap();

    let id = start_run(&pool, "2026-10-22 16:05:07 -05:00")
        .await
        .unwrap();
    let runs = recent_runs(&pool, 10).await.unwrap();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].outcome, "running");
    assert_eq!(runs[0].finished, "", "no end time while the run is going");

    finish_run(
        &pool,
        id,
        "2026-10-22 16:05:08 -05:00",
        "ok",
        "Processing finished in 0:00:01",
    )
    .await
    .unwrap();
    let runs = recent_runs(&pool, 10).await.unwrap();
    assert_eq!(
        runs.len(),
        1,
        "finishing updates the row instead of adding one"
    );
    assert_eq!(runs[0].outcome, "ok");
    assert_eq!(runs[0].finished, "2026-10-22 16:05:08 -05:00");
    assert_eq!(runs[0].message, "Processing finished in 0:00:01");
}

#[tokio::test]
async fn stale_running_runs_are_marked_interrupted() {
    let (config, _dir) = test_config();
    let pool = open(&config).await.unwrap();

    let _stale = start_run(&pool, "2026-10-22 16:05:07-05:00").await.unwrap();
    let done = start_run(&pool, "2026-10-22 17:05:07-05:00").await.unwrap();
    finish_run(&pool, done, "2026-10-22 17:05:08-05:00", "ok", "done")
        .await
        .unwrap();

    let now = timeutil::parse("2026-10-22 16:10:30-05:00").unwrap();
    assert_eq!(interrupt_stale_runs(&pool, now).await.unwrap(), 1);
    let runs = recent_runs(&pool, 10).await.unwrap();
    assert_eq!(runs[0].outcome, "ok", "a finished run is left alone");
    assert_eq!(runs[1].outcome, "error");
    assert_eq!(
        runs[1].message,
        "Interrupted: FediFetcher stopped while this run was in progress (running for 0:05:23)",
        "the message says how long the run had been going"
    );
    assert_eq!(
        interrupt_stale_runs(&pool, now).await.unwrap(),
        0,
        "nothing left to mark"
    );
}

#[tokio::test]
async fn private_posts_are_marked_once_and_survive_reopening() {
    let (config, _dir) = test_config();
    let pool = open(&config).await.unwrap();

    assert!(
        !is_private_post(&pool, "https://remote.example/@a/1")
            .await
            .unwrap()
    );
    assert!(
        mark_private_post(&pool, "https://remote.example/@a/1")
            .await
            .unwrap(),
        "the first mark records the post"
    );
    assert!(
        !mark_private_post(&pool, "https://remote.example/@a/1")
            .await
            .unwrap(),
        "marking again records nothing new"
    );
    assert!(
        is_private_post(&pool, "https://remote.example/@a/1")
            .await
            .unwrap()
    );
    assert!(
        !is_private_post(&pool, "https://remote.example/@a/2")
            .await
            .unwrap()
    );
    drop(pool);

    let pool = open(&config).await.unwrap();
    assert!(
        is_private_post(&pool, "https://remote.example/@a/1")
            .await
            .unwrap(),
        "the mark survives a reopen"
    );
}

#[tokio::test]
async fn opted_out_accounts_are_marked_once_and_survive_reopening() {
    let (config, _dir) = test_config();
    let pool = open(&config).await.unwrap();

    assert_eq!(
        opted_out_account_reason(&pool, "bot@remote.example")
            .await
            .unwrap(),
        None
    );
    assert!(
        mark_opted_out_account(&pool, "bot@remote.example", "it is not indexable")
            .await
            .unwrap(),
        "the first mark records the account"
    );
    assert!(
        !mark_opted_out_account(&pool, "bot@remote.example", "it is not indexable")
            .await
            .unwrap(),
        "marking again records nothing new"
    );
    assert_eq!(
        opted_out_account_reason(&pool, "bot@remote.example")
            .await
            .unwrap(),
        Some("it is not indexable".to_string())
    );
    drop(pool);

    let pool = open(&config).await.unwrap();
    assert_eq!(
        opted_out_account_reason(&pool, "bot@remote.example")
            .await
            .unwrap(),
        Some("it is not indexable".to_string()),
        "the mark survives a reopen"
    );
}

fn sample_trigger_post(uri: &str, source: &str) -> TriggerPost {
    TriggerPost {
        uri: uri.to_string(),
        url: uri.to_string(),
        acct: Some("a@remote.example".to_string()),
        author_url: Some("https://remote.example/@a".to_string()),
        replies_count: Some(3),
        created_at: Some("2026-10-22 16:05:07 -05:00".to_string()),
        in_reply_to_id: Some("https://remote.example/@a/0".to_string()),
        in_reply_to_account_id: Some("parent".to_string()),
        is_public: true,
        is_boost: false,
        mentions: Some("b@remote.example,c@remote.example".to_string()),
        source: source.to_string(),
    }
}

#[test]
fn trigger_post_from_post_reads_the_original() {
    use crate::users::User;
    let boosted = Post {
        url: "https://remote.example/@alice/1".to_string(),
        uri: "https://remote.example/@alice/1".to_string(),
        created_at: timeutil::now(),
        is_public: true,
        reblog: None,
        in_reply_to_id: Some("https://remote.example/@alice/0".to_string()),
        in_reply_to_account_id: Some("parent-id".to_string()),
        reply_count: Some(5),
        account: Some(User::new(
            "alice@remote.example",
            "https://remote.example/@alice",
        )),
        mentions: vec![User::new(
            "bob@remote.example",
            "https://remote.example/@bob",
        )],
    };
    let wrapper = Post {
        url: "https://home.example/@bob/2".to_string(),
        uri: "https://home.example/@bob/2".to_string(),
        created_at: timeutil::now(),
        is_public: true,
        reblog: Some(Box::new(boosted)),
        in_reply_to_id: None,
        in_reply_to_account_id: None,
        reply_count: None,
        account: None,
        mentions: vec![],
    };
    let record = TriggerPost::from_post(&wrapper, "home timeline");
    assert_eq!(record.uri, "https://remote.example/@alice/1");
    assert_eq!(record.url, "https://remote.example/@alice/1");
    assert_eq!(record.acct.as_deref(), Some("alice@remote.example"));
    assert_eq!(
        record.author_url.as_deref(),
        Some("https://remote.example/@alice")
    );
    assert_eq!(record.replies_count, Some(5));
    assert_eq!(
        record.in_reply_to_id.as_deref(),
        Some("https://remote.example/@alice/0")
    );
    assert_eq!(record.in_reply_to_account_id.as_deref(), Some("parent-id"));
    assert!(record.is_public);
    assert!(record.is_boost, "the scan was given a boost");
    assert_eq!(record.mentions.as_deref(), Some("bob@remote.example"));
    assert_eq!(record.source, "home timeline");
    assert!(record.created_at.is_some());
}

#[test]
fn trigger_post_from_post_records_a_plain_post_as_itself() {
    use crate::users::User;
    let post = Post {
        url: "https://remote.example/@alice/1".to_string(),
        uri: "https://remote.example/@alice/1".to_string(),
        created_at: timeutil::now(),
        is_public: false,
        reblog: None,
        in_reply_to_id: None,
        in_reply_to_account_id: None,
        reply_count: None,
        account: Some(User::new(
            "alice@remote.example",
            "https://remote.example/@alice",
        )),
        mentions: vec![],
    };
    let record = TriggerPost::from_post(&post, "replies");
    assert!(!record.is_public);
    assert!(!record.is_boost);
    assert_eq!(record.mentions, None, "no mentions records nothing");
    assert_eq!(record.in_reply_to_id, None);
}

/// A trigger_posts row as the test reads it: the scan's columns, in the
/// order the SELECT lists them.
type TriggerRow = (
    String,
    String,
    String,
    Option<i64>,
    Option<String>,
    Option<String>,
    bool,
    bool,
    Option<String>,
);

#[tokio::test]
async fn trigger_posts_are_recorded_once_and_refreshed() {
    let (config, _dir) = test_config();
    let pool = open(&config).await.unwrap();

    assert!(
        trigger_post(&pool, "https://remote.example/@a/1")
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        record_trigger_posts(
            &pool,
            &[sample_trigger_post(
                "https://remote.example/@a/1",
                "home timeline"
            )],
            100,
        )
        .await
        .unwrap(),
        1,
        "the first record is new"
    );
    let first = trigger_post(&pool, "https://remote.example/@a/1")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(first.source, "home timeline");
    assert_eq!(first.acct.as_deref(), Some("a@remote.example"));
    assert_eq!(first.replies_count, Some(3));

    // an earlier first_seen is kept; the sighting refreshes last_seen,
    // and the rest of the row follows the newest sighting
    let long_ago = timeutil::fmt_state_ts(timeutil::now() - time::Duration::days(30));
    sqlx::query("UPDATE trigger_posts SET first_seen = ?, last_seen = ? WHERE uri = ?")
        .bind(&long_ago)
        .bind(&long_ago)
        .bind("https://remote.example/@a/1")
        .execute(&pool)
        .await
        .unwrap();
    let mut updated = sample_trigger_post("https://remote.example/@a/1", "bookmarks");
    updated.replies_count = Some(7);
    assert_eq!(
        record_trigger_posts(&pool, &[updated], 100).await.unwrap(),
        0,
        "a sighting of a known post is not new"
    );
    let row: TriggerRow = sqlx::query_as(
            "SELECT first_seen, last_seen, source, replies_count, in_reply_to_id, author_url, is_public, is_boost, mentions FROM trigger_posts WHERE uri = ?",
        )
        .bind("https://remote.example/@a/1")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(row.0, long_ago, "first_seen is kept from the first record");
    assert_ne!(row.1, long_ago, "last_seen is refreshed");
    assert_eq!(
        row.2, "home timeline",
        "source is kept from the first record"
    );
    assert_eq!(
        row.3,
        Some(7),
        "the reply count follows the newest sighting"
    );
    assert_eq!(
        row.4.as_deref(),
        Some("https://remote.example/@a/0"),
        "the reply link follows the newest sighting"
    );
    assert_eq!(row.5.as_deref(), Some("https://remote.example/@a"));
    assert!(row.6, "the visibility follows the newest sighting");
    assert!(!row.7);
    assert_eq!(row.8.as_deref(), Some("b@remote.example,c@remote.example"));
}

#[tokio::test]
async fn recording_trigger_posts_keeps_only_the_newest() {
    let (config, _dir) = test_config();
    let pool = open(&config).await.unwrap();

    let old = timeutil::fmt_state_ts(timeutil::now() - time::Duration::days(10));
    for n in 0..5 {
        sqlx::query(
                "INSERT INTO trigger_posts (uri, url, source, first_seen, last_seen) VALUES (?, ?, 'test', ?, ?)",
            )
            .bind(format!("https://remote.example/{n}"))
            .bind(format!("https://remote.example/{n}"))
            .bind(&old)
            .bind(&old)
            .execute(&pool)
            .await
            .unwrap();
    }
    record_trigger_posts(
        &pool,
        &[sample_trigger_post("https://remote.example/new", "test")],
        3,
    )
    .await
    .unwrap();

    assert_eq!(table_count(&pool, "trigger_posts").await.unwrap(), 3);
    assert!(
        trigger_post(&pool, "https://remote.example/new")
            .await
            .unwrap()
            .is_some(),
        "the freshly recorded post survives"
    );
    assert!(
        trigger_post(&pool, "https://remote.example/4")
            .await
            .unwrap()
            .is_none(),
        "the oldest entries are pruned"
    );
    // recording nothing records nothing, and prunes nothing
    assert_eq!(record_trigger_posts(&pool, &[], 3).await.unwrap(), 0);
    assert_eq!(table_count(&pool, "trigger_posts").await.unwrap(), 3);
}

#[tokio::test]
async fn a_post_seen_twice_in_one_scan_counts_as_new_once() {
    let (config, _dir) = test_config();
    let pool = open(&config).await.unwrap();

    let post = sample_trigger_post("https://remote.example/@a/1", "home timeline");
    assert_eq!(
        record_trigger_posts(&pool, &[post.clone(), post], 100)
            .await
            .unwrap(),
        1,
        "a post repeated in one scan is new once"
    );
}

/// A pool on the test database with its own busy timeout, so a write
/// fails at once instead of waiting out another connection's lock.
async fn pool_at(path: &std::path::Path, busy_timeout: std::time::Duration) -> SqlitePool {
    SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(sqlite_options(path, busy_timeout))
        .await
        .unwrap()
}

#[tokio::test]
async fn a_held_write_lock_is_recognized_as_locked() {
    let (_config, dir) = test_config();
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("fedifetcher.db");
    let writer = pool_at(&path, std::time::Duration::from_secs(30)).await;
    create_schema(&writer).await.unwrap();
    let reader = pool_at(&path, std::time::Duration::ZERO).await;

    let insert = "INSERT INTO logs (ts, level, target, message) VALUES ('t', 'INFO', 'test', 'm')";

    // SQLITE_BUSY: a write while another connection holds the write lock
    let mut tx = writer.begin().await.unwrap();
    sqlx::query(insert).execute(&mut *tx).await.unwrap();
    let err = sqlx::query(insert).execute(&reader).await.unwrap_err();
    assert!(is_locked(&err), "a held lock reads as locked: {err}");
    tx.commit().await.unwrap();

    // SQLITE_BUSY_SNAPSHOT: a transaction that read, then found another
    // connection had written since, cannot upgrade its snapshot to a write
    let mut tx = reader.begin().await.unwrap();
    let _: Option<(i64,)> = sqlx::query_as("SELECT COUNT(*) FROM logs")
        .fetch_optional(&mut *tx)
        .await
        .unwrap();
    sqlx::query(insert).execute(&writer).await.unwrap();
    let err = sqlx::query(insert).execute(&mut *tx).await.unwrap_err();
    assert!(is_locked(&err), "a stale snapshot reads as locked: {err}");
    // the pool has one connection; the open transaction holds it
    drop(tx);

    // any other database error is not a held lock
    let err = sqlx::query("INSERT INTO logs (ts) VALUES ('t')")
        .execute(&reader)
        .await
        .unwrap_err();
    assert!(
        matches!(err, sqlx::Error::Database(_)),
        "the failed insert is a database error: {err}"
    );
    assert!(!is_locked(&err), "a constraint error is not a lock: {err}");
}

/// The failure a run logs when it cannot record its trigger posts: the
/// web UI and the log writer are writing to the same database all the
/// while, and a write that arrives mid-transaction fails at once.
#[tokio::test]
async fn a_write_is_retried_while_another_connection_holds_the_lock() {
    let (_config, dir) = test_config();
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("fedifetcher.db");
    let pool = pool_at(&path, std::time::Duration::ZERO).await;
    create_schema(&pool).await.unwrap();
    let other = pool_at(&path, std::time::Duration::from_secs(30)).await;

    // another connection holds the write lock until the test releases it
    let (release, held) = tokio::sync::oneshot::channel::<()>();
    let holder = tokio::spawn(async move {
        let mut tx = other.begin().await.unwrap();
        sqlx::query(
            "INSERT INTO logs (ts, level, target, message) VALUES ('t', 'INFO', 'test', 'held')",
        )
        .execute(&mut *tx)
        .await
        .unwrap();
        let _ = held.await;
        tx.commit().await.unwrap();
    });

    // the write fails at once while the lock is held, and succeeds once
    // it is released
    let writing = pool.clone();
    let write = tokio::spawn(async move {
        record_trigger_posts(
            &writing,
            &[sample_trigger_post(
                "https://remote.example/@a/1",
                "home timeline",
            )],
            100,
        )
        .await
    });
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    let _ = release.send(());
    assert_eq!(write.await.unwrap().unwrap(), 1, "the post is new");
    holder.await.unwrap();
    assert!(
        trigger_post(&pool, "https://remote.example/@a/1")
            .await
            .unwrap()
            .is_some(),
        "the post is recorded"
    );
}

/// Open a database whose trigger_posts table predates the columns that
/// record what a scan pulled in on each post, and check the table is
/// reshaped with its rows kept.
#[tokio::test]
async fn an_older_trigger_posts_table_is_migrated() {
    let (config, dir) = test_config();
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("fedifetcher.db");
    let setup = SqlitePoolOptions::new()
        .connect_with(
            SqliteConnectOptions::new()
                .filename(&path)
                .create_if_missing(true),
        )
        .await
        .unwrap();
    sqlx::query(
            "CREATE TABLE trigger_posts (
                uri TEXT PRIMARY KEY, url TEXT NOT NULL, acct TEXT, replies_count INTEGER,
                created_at TEXT, source TEXT NOT NULL, first_seen TEXT NOT NULL, last_seen TEXT NOT NULL
            )",
        )
        .execute(&setup)
        .await
        .unwrap();
    sqlx::query(
            "INSERT INTO trigger_posts (uri, url, source, first_seen, last_seen) VALUES (?, ?, 'home timeline', 'old', 'old')",
        )
        .bind("https://remote.example/@a/1")
        .bind("https://remote.example/@a/1")
        .execute(&setup)
        .await
        .unwrap();
    drop(setup);

    let pool = open(&config).await.unwrap();
    let columns = table_columns(&pool, "trigger_posts").await.unwrap();
    for expected in [
        "in_reply_to_id",
        "in_reply_to_account_id",
        "is_public",
        "is_boost",
        "author_url",
        "mentions",
    ] {
        assert!(columns.contains(&expected.to_string()), "{expected} added");
    }
    let old = trigger_post(&pool, "https://remote.example/@a/1")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(old.source, "home timeline", "the old row is kept");
    assert!(old.is_public, "an old row reads as a public post");
    assert!(!old.is_boost);
    assert_eq!(old.in_reply_to_id, None);
}

#[tokio::test]
async fn seen_hosts_are_stored_in_columns() {
    let (config, _dir) = test_config();
    let pool = open(&config).await.unwrap();

    let mut state = State::default();
    state.seen_hosts_mut().add(
        "remote.example",
        serde_json::json!({
            "webserver": "remote.example", "software": "mastodon", "version": "4.3.0",
            "rawnodeinfo": {"software": {"name": "mastodon", "version": "4.3.0"}},
            "last_checked": timeutil::fmt_state_ts(timeutil::now()),
            "mastodonApiSupport": true, "misskeyApiSupport": false,
            "lemmyApiSupport": false, "peertubeApiSupport": false,
        }),
    );
    state.seen_hosts_mut().add(
        "down.example",
        crate::servers::failed_lookup_state("prohibited by robots.txt"),
    );
    save_state(&pool, &state, 1000).await.unwrap();

    let columns = table_columns(&pool, "seen_hosts").await.unwrap();
    assert!(columns.contains(&"software".to_string()));
    assert!(columns.contains(&"mastodon_api".to_string()));
    assert!(
        !columns.contains(&"info".to_string()),
        "no JSON blob column"
    );

    let row: (Option<String>, Option<String>, bool, bool, String) = sqlx::query_as(
            "SELECT software, version, mastodon_api, peertube_api, raw_nodeinfo FROM seen_hosts WHERE host = 'remote.example'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(row.0.as_deref(), Some("mastodon"));
    assert_eq!(row.1.as_deref(), Some("4.3.0"));
    assert!(row.2);
    assert!(!row.3);
    assert!(
        row.4.contains("mastodon"),
        "the nodeinfo document is kept whole"
    );

    // and the state reads back as it was
    let loaded = load_state(&pool).await.unwrap();
    let info =
        crate::servers::ServerInfo::from_state(loaded.seen_hosts().get("remote.example").unwrap())
            .unwrap();
    assert_eq!(info.software, "mastodon");
    assert_eq!(info.version, "4.3.0");
    assert!(info.supports(crate::servers::ApiFlavor::Mastodon));
    assert!(
        crate::servers::ServerInfo::from_state(loaded.seen_hosts().get("down.example").unwrap())
            .is_none(),
        "a failed lookup still reads as a failure"
    );
    assert_eq!(
        loaded.seen_hosts().get("down.example").unwrap()["error"],
        serde_json::json!("prohibited by robots.txt")
    );
}

/// Open a database whose seen_hosts table kept every field in one JSON
/// column, and check the rows are broken out into columns.
#[tokio::test]
async fn an_older_seen_hosts_table_is_migrated_to_columns() {
    let (config, dir) = test_config();
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("fedifetcher.db");
    let setup = SqlitePoolOptions::new()
        .connect_with(
            SqliteConnectOptions::new()
                .filename(&path)
                .create_if_missing(true),
        )
        .await
        .unwrap();
    sqlx::query("CREATE TABLE seen_hosts (host TEXT PRIMARY KEY, info TEXT NOT NULL)")
        .execute(&setup)
        .await
        .unwrap();
    let success = serde_json::json!({
        "webserver": "remote.example", "software": "mastodon", "version": "4.3.0",
        "rawnodeinfo": {"software": {"name": "mastodon", "version": "4.3.0"}},
        "last_checked": timeutil::fmt_state_ts(timeutil::now()),
        "mastodonApiSupport": true, "misskeyApiSupport": false,
        "lemmyApiSupport": false, "peertubeApiSupport": false,
    });
    let failure = crate::servers::failed_lookup_state("prohibited by robots.txt");
    sqlx::query("INSERT INTO seen_hosts (host, info) VALUES (?, ?)")
        .bind("remote.example")
        .bind(success.to_string())
        .execute(&setup)
        .await
        .unwrap();
    sqlx::query("INSERT INTO seen_hosts (host, info) VALUES (?, ?)")
        .bind("down.example")
        .bind(failure.to_string())
        .execute(&setup)
        .await
        .unwrap();
    drop(setup);

    let pool = open(&config).await.unwrap();
    let columns = table_columns(&pool, "seen_hosts").await.unwrap();
    assert!(
        columns.contains(&"software".to_string()),
        "the fields are columns now"
    );
    assert!(
        !columns.contains(&"info".to_string()),
        "the JSON blob is gone"
    );

    let row: (Option<String>, Option<String>, bool, Option<String>) = sqlx::query_as(
            "SELECT software, version, mastodon_api, error FROM seen_hosts WHERE host = 'remote.example'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(row.0.as_deref(), Some("mastodon"));
    assert_eq!(row.1.as_deref(), Some("4.3.0"));
    assert!(row.2);
    assert_eq!(row.3, None);
    let row: (Option<String>, Option<String>) =
        sqlx::query_as("SELECT software, error FROM seen_hosts WHERE host = 'down.example'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(row.0, None);
    assert_eq!(row.1.as_deref(), Some("prohibited by robots.txt"));

    // and the migrated rows read back as the state they were
    let state = load_state(&pool).await.unwrap();
    let info =
        crate::servers::ServerInfo::from_state(state.seen_hosts().get("remote.example").unwrap())
            .unwrap();
    assert_eq!(info.software, "mastodon");
    assert!(info.supports(crate::servers::ApiFlavor::Mastodon));
    assert!(
        crate::servers::ServerInfo::from_state(state.seen_hosts().get("down.example").unwrap())
            .is_none()
    );
}

#[tokio::test]
async fn the_database_survives_reopening() {
    let (config, _dir) = test_config();
    let pool = open(&config).await.unwrap();
    insert_log(&pool, &log_row("remember me")).await.unwrap();
    drop(pool);

    let pool = open(&config).await.unwrap();
    let lines = recent_logs(&pool, 10, LogSort::Time, LogOrder::Desc, None)
        .await
        .unwrap();
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].message, "remember me");
}

#[tokio::test]
async fn the_data_page_lists_tables_columns_and_counts() {
    let (config, _dir) = test_config();
    let pool = open(&config).await.unwrap();

    let tables = list_tables(&pool).await.unwrap();
    for expected in [
        "known_context_posts",
        "logs",
        "opted_out_accounts",
        "private_posts",
        "runs",
    ] {
        assert!(tables.contains(&expected.to_string()), "{expected} listed");
    }
    assert!(
        !tables.iter().any(|name| name.starts_with("sqlite_")),
        "internal tables are left out"
    );

    let columns = table_columns(&pool, "logs").await.unwrap();
    assert_eq!(columns, ["id", "ts", "level", "target", "message"]);

    assert_eq!(table_count(&pool, "logs").await.unwrap(), 0);
    insert_log(&pool, &log_row("hello")).await.unwrap();
    assert_eq!(table_count(&pool, "logs").await.unwrap(), 1);
}

#[tokio::test]
async fn table_rows_returns_text_sorted_by_the_asked_column() {
    let (config, _dir) = test_config();
    let pool = open(&config).await.unwrap();
    let columns = table_columns(&pool, "logs").await.unwrap();

    let mut error = log_row("an error");
    error.level = "ERROR".to_string();
    let mut info = log_row("an info");
    info.level = "INFO".to_string();
    insert_log(&pool, &error).await.unwrap();
    insert_log(&pool, &info).await.unwrap();

    // every value comes back as text, newest first by default of the ask
    let rows = table_rows(&pool, "logs", &columns, "id", LogOrder::Desc, 10)
        .await
        .unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0][0].as_deref(), Some("2"), "the id is text");
    assert_eq!(rows[0][4].as_deref(), Some("an info"));
    assert_eq!(rows[1][4].as_deref(), Some("an error"));

    // sorted by level, ascending: ERROR before INFO
    let rows = table_rows(&pool, "logs", &columns, "level", LogOrder::Asc, 10)
        .await
        .unwrap();
    assert_eq!(rows[0][4].as_deref(), Some("an error"));
    assert_eq!(rows[1][4].as_deref(), Some("an info"));

    // the limit applies after sorting
    let rows = table_rows(&pool, "logs", &columns, "level", LogOrder::Asc, 1)
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][4].as_deref(), Some("an error"));
}

#[tokio::test]
async fn table_rows_renders_null_as_none_and_integers_as_text() {
    let (config, _dir) = test_config();
    let pool = open(&config).await.unwrap();
    sqlx::query("CREATE TABLE scratch (name TEXT, note TEXT)")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO scratch (name, note) VALUES ('x', NULL), ('y', 'text'), ('z', 42)")
        .execute(&pool)
        .await
        .unwrap();

    let columns = table_columns(&pool, "scratch").await.unwrap();
    assert_eq!(columns, ["name", "note"]);
    let rows = table_rows(&pool, "scratch", &columns, "name", LogOrder::Asc, 10)
        .await
        .unwrap();
    assert_eq!(rows[0], [Some("x".to_string()), None], "SQL NULL is None");
    assert_eq!(rows[1], [Some("y".to_string()), Some("text".to_string())]);
    assert_eq!(rows[2][1].as_deref(), Some("42"), "integers are text");
}

#[tokio::test]
async fn new_known_context_posts_are_reported_as_new() {
    let (config, _dir) = test_config();
    let pool = open(&config).await.unwrap();

    let urls = vec!["https://remote.example/@a/1".to_string()];
    let previous = record_known_context_posts(&pool, &urls).await.unwrap();
    assert_eq!(previous, vec![None], "the post is new");
    assert_eq!(table_count(&pool, "known_context_posts").await.unwrap(), 1);

    // a sighting with nothing found records nothing
    assert!(
        record_known_context_posts(&pool, &[])
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn known_context_posts_keep_first_seen_and_refresh_last_seen() {
    let (config, _dir) = test_config();
    let pool = open(&config).await.unwrap();

    let uri = "https://remote.example/@a/1";
    let long_ago = timeutil::fmt_state_ts(timeutil::now() - time::Duration::days(30));
    sqlx::query("INSERT INTO known_context_posts (uri, first_seen, last_seen) VALUES (?, ?, ?)")
        .bind(uri)
        .bind(&long_ago)
        .bind(&long_ago)
        .execute(&pool)
        .await
        .unwrap();

    let previous = record_known_context_posts(&pool, &[uri.to_string()])
        .await
        .unwrap();
    assert_eq!(
        previous,
        vec![Some(long_ago.clone())],
        "the earlier last_seen is reported, so the caller can tell how recent it was"
    );

    let row: (String, String) =
        sqlx::query_as("SELECT first_seen, last_seen FROM known_context_posts WHERE uri = ?")
            .bind(uri)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(row.0, long_ago, "first_seen is kept from the first record");
    assert_ne!(row.1, long_ago, "last_seen is refreshed");
    assert!(
        timeutil::parse(&row.1).is_some(),
        "the refresh is a real timestamp"
    );
}

#[tokio::test]
async fn table_rows_rejects_names_that_are_not_identifiers() {
    let (config, _dir) = test_config();
    let pool = open(&config).await.unwrap();
    let columns = table_columns(&pool, "logs").await.unwrap();

    assert!(
        table_rows(
            &pool,
            "logs; DROP TABLE logs",
            &columns,
            "id",
            LogOrder::Asc,
            10
        )
        .await
        .is_err()
    );
    assert!(
        table_rows(
            &pool,
            "logs",
            &columns,
            "id; DROP TABLE logs",
            LogOrder::Asc,
            10
        )
        .await
        .is_err()
    );
    assert!(
        table_rows(&pool, "logs", &columns, "not_a_column", LogOrder::Asc, 10)
            .await
            .is_err(),
        "a sort column must be a real column"
    );
    assert!(
        table_columns(&pool, "logs\"; DROP TABLE logs")
            .await
            .is_err()
    );
    assert!(table_count(&pool, "").await.is_err());
    // and the table survives
    assert!(table_count(&pool, "logs").await.is_ok());
}

#[tokio::test]
async fn config_round_trips_through_the_database() {
    let (config, _dir) = test_config();
    let pool = open(&config).await.unwrap();

    assert!(load_config(&pool).await.unwrap().is_empty());

    let mut values = RawValues::new();
    values.insert(
        "server".to_string(),
        Value::String("example.social".to_string()),
    );
    values.insert(
        "access_tokens".to_string(),
        Value::Array(vec![Value::String("tok".to_string())]),
    );
    values.insert("max_followers".to_string(), Value::from(5));
    values.insert("from_lists".to_string(), Value::Bool(true));
    save_config(&pool, &values).await.unwrap();
    assert_eq!(load_config(&pool).await.unwrap(), values);

    // saving again replaces: options absent from the new set are cleared
    let mut cleared = RawValues::new();
    cleared.insert(
        "server".to_string(),
        Value::String("other.example".to_string()),
    );
    save_config(&pool, &cleared).await.unwrap();
    assert_eq!(load_config(&pool).await.unwrap(), cleared);
}

#[tokio::test]
async fn state_round_trips_through_the_database() {
    let (config, _dir) = test_config();
    let pool = open(&config).await.unwrap();

    let mut state = State::default();
    state
        .seen_urls_mut()
        .add("https://remote.example/@a/1", None);
    state.known_followings_mut().add("b@remote.example", None);
    state
        .recently_checked_users_mut()
        .add("c@remote.example", None);
    state.seen_hosts_mut().add(
        "remote.example",
        serde_json::json!({
            "software": "mastodon", "version": "4.3",
            "last_checked": timeutil::fmt_state_ts(timeutil::now()),
            "mastodonApiSupport": true, "peertubeApiSupport": false,
        }),
    );
    state
        .recently_checked_context_mut()
        .mark_fetched("uri", timeutil::now());
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
    save_state(&pool, &state, 100).await.unwrap();

    let loaded = load_state(&pool).await.unwrap();
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
        Some(&None),
        "a failed lookup is remembered as a NULL row"
    );
    // timestamps survive the round trip
    assert_eq!(
        loaded.seen_urls().get("https://remote.example/@a/1"),
        state.seen_urls().get("https://remote.example/@a/1")
    );
}

#[tokio::test]
async fn saving_state_keeps_only_the_newest_entries() {
    let (config, _dir) = test_config();
    let pool = open(&config).await.unwrap();

    let mut state = State::default();
    for n in 0..5 {
        state
            .seen_urls_mut()
            .add(&format!("https://remote.example/{n}"), None);
    }
    save_state(&pool, &state, 3).await.unwrap();

    let loaded = load_state(&pool).await.unwrap();
    assert_eq!(loaded.seen_urls().len(), 3);
    assert!(!loaded.seen_urls().contains("https://remote.example/0"));
    assert!(!loaded.seen_urls().contains("https://remote.example/1"));
    assert!(loaded.seen_urls().contains("https://remote.example/4"));
}

#[tokio::test]
async fn robots_cache_round_trips_and_discards_stale_entries() {
    let (config, _dir) = test_config();
    let pool = open(&config).await.unwrap();

    assert_eq!(
        robots_cached(&pool, "https://a.example/robots.txt")
            .await
            .unwrap(),
        None
    );
    robots_store(&pool, "https://a.example/robots.txt", "allow")
        .await
        .unwrap();
    robots_store(&pool, "https://b.example/robots.txt", "allow")
        .await
        .unwrap();
    assert_eq!(
        robots_cached(&pool, "https://a.example/robots.txt")
            .await
            .unwrap(),
        Some("allow".to_string())
    );

    // storing again replaces the earlier copy
    robots_store(&pool, "https://a.example/robots.txt", "deny")
        .await
        .unwrap();
    assert_eq!(
        robots_cached(&pool, "https://a.example/robots.txt")
            .await
            .unwrap(),
        Some("deny".to_string())
    );

    // an entry older than the cutoff is discarded
    let old = timeutil::fmt_state_ts(timeutil::now() - time::Duration::days(2));
    sqlx::query("UPDATE robots_cache SET fetched_at = ? WHERE url = ?")
        .bind(&old)
        .bind("https://a.example/robots.txt")
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        robots_discard_stale(&pool, time::Duration::days(1))
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        robots_cached(&pool, "https://a.example/robots.txt")
            .await
            .unwrap(),
        None
    );
    assert_eq!(
        robots_cached(&pool, "https://b.example/robots.txt")
            .await
            .unwrap(),
        Some("allow".to_string()),
        "a fresh entry is kept"
    );
}

#[tokio::test]
async fn table_rows_with_no_columns_asked_returns_no_rows() {
    let (config, _dir) = test_config();
    let pool = open(&config).await.unwrap();
    let rows = table_rows(&pool, "logs", &[], "ts", LogOrder::Desc, 10)
        .await
        .unwrap();
    assert!(rows.is_empty());
}

#[test]
fn a_non_database_error_is_never_a_lock_error() {
    assert!(!is_locked(&sqlx::Error::RowNotFound));
}

#[test]
fn disk_usage_reports_the_database_and_its_wal() {
    let (config, dir) = test_config();
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("fedifetcher.db"), vec![0u8; 4096]).unwrap();
    std::fs::write(dir.join("fedifetcher.db-wal"), vec![0u8; 1024]).unwrap();

    let usage = disk_usage(&config).expect("the database file exists");
    assert_eq!(usage.database, 4096);
    assert_eq!(usage.wal, Some(1024));

    // no wal file reads as no wal
    std::fs::remove_file(dir.join("fedifetcher.db-wal")).unwrap();
    let usage = disk_usage(&config).unwrap();
    assert_eq!(usage.wal, None);

    // no database at all reads as nothing to report
    std::fs::remove_file(dir.join("fedifetcher.db")).unwrap();
    assert!(disk_usage(&config).is_none());
}
