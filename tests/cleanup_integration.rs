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
