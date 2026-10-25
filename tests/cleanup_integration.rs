//! Cleaning the database from the debug page: everything but the
//! configuration and the OAuth rows is emptied, and the freed space is
//! returned to the filesystem.

mod common;

use fedifetcher::config::Config;
use fedifetcher::db;
use fedifetcher::timeutil;
use sqlx::Row;

/// A timestamp right now, as the state spells it.
fn now_ts() -> String {
    timeutil::fmt_state_ts(timeutil::now())
}

/// The `i`th of 10 timestamps, minutes apart, so the oldest is unambiguous.
fn ts_i(i: i64) -> String {
    timeutil::fmt_state_ts(timeutil::now() - time::Duration::minutes(100 - i))
}

async fn count(pool: &sqlx::SqlitePool, table: &str) -> i64 {
    db::table_count(pool, table).await.unwrap()
}

/// A fresh database with one row in every table.
async fn seeded_db(config: &Config) -> sqlx::SqlitePool {
    let pool = db::open(config).await.unwrap();
    sqlx::query("INSERT INTO config (key, value) VALUES ('server', 'example.social')")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO oauth_accounts (token, server, acct, avatar, connected_at)
         VALUES ('tok', 'example.social', 'me', '', '2026-01-01')",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO oauth_apps (server, client_id, client_secret, registered_at)
         VALUES ('example.social', 'id', 'secret', '2026-01-01')",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO runs (started, finished, outcome, message) VALUES ('s', 'f', 'ok', 'm')",
    )
    .execute(&pool)
    .await
    .unwrap();
    db::insert_log(
        &pool,
        &db::LogRow {
            ts: now_ts(),
            level: "INFO".to_string(),
            target: "t".to_string(),
            message: "a line".to_string(),
        },
    )
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO opted_out_accounts (acct, reason, first_seen) VALUES ('bot', 'nobot', ?1)",
    )
    .bind(now_ts())
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query("INSERT INTO private_posts (uri, first_marked) VALUES ('u1', ?1)")
        .bind(now_ts())
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO trigger_posts (uri, url, source, first_seen, last_seen) VALUES ('u1', 'u1', 'test', ?1, ?1)")
        .bind(now_ts())
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO known_context_posts (uri, first_seen, last_seen) VALUES ('u1', ?1, ?1)",
    )
    .bind(now_ts())
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query("INSERT INTO replied_toots (url, remote_url, server, post_id) VALUES ('u1', 'r1', 's', '1')")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO known_followings (acct, first_seen) VALUES ('me', ?1)")
        .bind(now_ts())
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO seen_urls (uri, first_seen) VALUES ('u1', ?1)")
        .bind(now_ts())
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO recently_checked_users (acct, first_seen) VALUES ('me', ?1)")
        .bind(now_ts())
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO context_cache (uri, last_seen, created_at) VALUES ('u1', ?1, ?1)")
        .bind(now_ts())
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO robots_cache (url, text, fetched_at) VALUES ('r.txt', '', ?1)")
        .bind(now_ts())
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO seen_hosts (host, webserver, software, version, mastodon_api,
         misskey_api, lemmy_api, peertube_api, last_checked, error, raw_nodeinfo)
         VALUES ('h.example', 'h.example', 'mastodon', '4', 1, 0, 0, 0, ?1, NULL, '{}')",
    )
    .bind(now_ts())
    .execute(&pool)
    .await
    .unwrap();
    pool
}

/// Every table but the configuration's and the OAuth ones is emptied, and
/// the report says how much each held.
#[tokio::test]
async fn cleanup_empties_every_table_but_the_kept_three() {
    let dir = common::tmp_dir();
    let config = common::test_config(&dir, "example.social");
    let pool = seeded_db(&config).await;

    let report = db::cleanup(&pool).await.unwrap();

    for table in [
        "logs",
        "runs",
        "private_posts",
        "opted_out_accounts",
        "known_context_posts",
        "trigger_posts",
        "seen_urls",
        "known_followings",
        "recently_checked_users",
        "context_cache",
        "replied_toots",
        "robots_cache",
        "seen_hosts",
    ] {
        assert_eq!(count(&pool, table).await, 0, "the {table} table is emptied");
    }
    assert_eq!(count(&pool, "config").await, 1, "the config records stay");
    assert_eq!(
        count(&pool, "oauth_accounts").await,
        1,
        "the connected accounts stay"
    );
    assert_eq!(
        count(&pool, "oauth_apps").await,
        1,
        "the registered OAuth apps stay"
    );

    let removed: Vec<(&str, u64)> = report.removed;
    assert!(
        removed.contains(&("log line", 1)) && removed.contains(&("seen URL", 1)),
        "the report counts what each emptied table held: {removed:?}"
    );
    assert!(
        !removed.iter().any(|(label, _)| *label == "config record"),
        "the kept tables are not in the report: {removed:?}"
    );
}

/// The kept rows survive with their content intact.
#[tokio::test]
async fn the_kept_rows_survive_intact() {
    let dir = common::tmp_dir();
    let config = common::test_config(&dir, "example.social");
    let pool = seeded_db(&config).await;

    db::cleanup(&pool).await.unwrap();

    let server: String = sqlx::query("SELECT value FROM config WHERE key = 'server'")
        .fetch_one(&pool)
        .await
        .unwrap()
        .get("value");
    assert_eq!(server, "example.social");
    let acct: String = sqlx::query("SELECT acct FROM oauth_accounts WHERE token = 'tok'")
        .fetch_one(&pool)
        .await
        .unwrap()
        .get("acct");
    assert_eq!(acct, "me");
}

/// A cleanup on an empty database is a success that removed nothing.
#[tokio::test]
async fn cleaning_an_empty_database_removes_nothing() {
    let dir = common::tmp_dir();
    let config = common::test_config(&dir, "example.social");
    let pool = db::open(&config).await.unwrap();

    let report = db::cleanup(&pool).await.unwrap();

    assert!(report.removed.is_empty(), "{:?}", report.removed);
}

/// A trim removes the oldest tenth of each cleanable table, keeping the
/// newest records.
#[tokio::test]
async fn a_trim_removes_the_oldest_tenth_of_each_table() {
    let dir = common::tmp_dir();
    let config = common::test_config(&dir, "example.social");
    let pool = seeded_db(&config).await;

    // nine rows more in every cleanable table, oldest first — with the
    // seed row that makes ten; the kept tables get one row each, which a
    // trim must not touch
    for i in 0..9i64 {
        db::insert_log(
            &pool,
            &db::LogRow {
                ts: ts_i(i),
                level: "INFO".to_string(),
                target: "t".to_string(),
                message: format!("line {i}"),
            },
        )
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO runs (started, finished, outcome, message) VALUES (?1, 'f', 'ok', 'm')",
        )
        .bind(ts_i(i))
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query("INSERT INTO seen_urls (uri, first_seen) VALUES (?1, ?2)")
            .bind(format!("su{i}"))
            .bind(ts_i(i))
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO known_followings (acct, first_seen) VALUES (?1, ?2)")
            .bind(format!("f{i}"))
            .bind(ts_i(i))
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO recently_checked_users (acct, first_seen) VALUES (?1, ?2)")
            .bind(format!("r{i}"))
            .bind(ts_i(i))
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO seen_hosts (host, webserver, software, version, mastodon_api,
             misskey_api, lemmy_api, peertube_api, last_checked, error, raw_nodeinfo)
             VALUES (?1, ?1, 'mastodon', '4', 1, 0, 0, 0, ?2, NULL, '{}')",
        )
        .bind(format!("h{i}.example"))
        .bind(ts_i(i))
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query("INSERT INTO context_cache (uri, last_seen, created_at) VALUES (?1, ?2, ?2)")
            .bind(format!("c{i}"))
            .bind(ts_i(i))
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO replied_toots (url, remote_url, server, post_id) VALUES (?1, 'r', 's', '1')")
            .bind(format!("t{i}"))
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO robots_cache (url, text, fetched_at) VALUES (?1, '', ?2)")
            .bind(format!("robots{i}.txt"))
            .bind(ts_i(i))
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO trigger_posts (uri, url, source, first_seen, last_seen) VALUES (?1, ?1, 'test', ?2, ?2)")
            .bind(format!("p{i}"))
            .bind(ts_i(i))
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO known_context_posts (uri, first_seen, last_seen) VALUES (?1, ?2, ?2)",
        )
        .bind(format!("k{i}"))
        .bind(ts_i(i))
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query("INSERT INTO private_posts (uri, first_marked) VALUES (?1, ?2)")
            .bind(format!("v{i}"))
            .bind(ts_i(i))
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO opted_out_accounts (acct, reason, first_seen) VALUES (?1, 'nobot', ?2)",
        )
        .bind(format!("o{i}"))
        .bind(ts_i(i))
        .execute(&pool)
        .await
        .unwrap();
    }

    let report = db::trim(&pool).await.unwrap();

    for table in [
        "logs",
        "runs",
        "seen_urls",
        "known_followings",
        "recently_checked_users",
        "seen_hosts",
        "context_cache",
        "replied_toots",
        "robots_cache",
        "trigger_posts",
        "known_context_posts",
        "private_posts",
        "opted_out_accounts",
    ] {
        assert_eq!(
            count(&pool, table).await,
            9,
            "the {table} table keeps its newest nine of ten"
        );
    }
    assert_eq!(count(&pool, "config").await, 1, "the config records stay");
    assert_eq!(
        count(&pool, "oauth_accounts").await,
        1,
        "the connected accounts stay"
    );
    assert_eq!(
        count(&pool, "oauth_apps").await,
        1,
        "the registered OAuth apps stay"
    );

    // the logs go by insertion order: the seed row (the first inserted)
    // is the oldest, so it went, and the newest of the loop's rows stayed
    let messages: Vec<String> =
        db::recent_logs(&pool, 100, db::LogSort::Time, db::LogOrder::Asc, None)
            .await
            .unwrap()
            .into_iter()
            .map(|row| row.message)
            .collect();
    assert!(
        !messages.contains(&"a line".to_string()) && messages.contains(&"line 8".to_string()),
        "the oldest log line went, the newest stayed: {messages:?}"
    );

    // the report counts one removal per table
    assert_eq!(
        report.removed.len(),
        13,
        "every cleanable table contributed: {:?}",
        report.removed
    );
    assert!(
        report.removed.iter().all(|(_, count)| *count == 1),
        "a tenth of ten is one: {:?}",
        report.removed
    );
}

/// A trim of a table with fewer than ten rows removes nothing: a tenth of
/// nine rounds to nothing.
#[tokio::test]
async fn a_trim_of_a_small_table_removes_nothing() {
    let dir = common::tmp_dir();
    let config = common::test_config(&dir, "example.social");
    let pool = seeded_db(&config).await;

    let report = db::trim(&pool).await.unwrap();

    assert!(report.removed.is_empty(), "{:?}", report.removed);
    assert_eq!(count(&pool, "seen_urls").await, 1);
}
