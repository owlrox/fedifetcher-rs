//! Cleaning the database from the debug page: only what the app's own
//! retention rules would remove is removed, and the records that matter —
//! the configuration, the OAuth rows, the run history, the accounts'
//! recorded wishes — are kept.

mod common;

use fedifetcher::config::Config;
use fedifetcher::db;
use fedifetcher::store::StateStore;
use fedifetcher::timeutil;
use sqlx::Row;

/// A timestamp well in the past, as the state spells it.
fn old_ts() -> String {
    "2026-01-01 00:00:00+00:00".to_string()
}

/// A timestamp right now, as the state spells it.
fn now_ts() -> String {
    timeutil::fmt_state_ts(timeutil::now())
}

async fn count(pool: &sqlx::SqlitePool, table: &str) -> i64 {
    db::table_count(pool, table).await.unwrap()
}

/// A fresh database with one row in every table cleanup may touch or must
/// keep.
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
        .bind(old_ts())
        .execute(&pool)
        .await
        .unwrap();
    pool
}

/// Log lines beyond the retention are pruned, the newest kept.
#[tokio::test]
async fn cleanup_prunes_the_logs_to_the_retention() {
    let dir = common::tmp_dir();
    let config = common::test_config(&dir, "example.social");
    let pool = seeded_db(&config).await;
    for i in 0..10 {
        db::insert_log(
            &pool,
            &db::LogRow {
                ts: now_ts(),
                level: "INFO".to_string(),
                target: "t".to_string(),
                message: format!("line {i}"),
            },
        )
        .await
        .unwrap();
    }

    let report = StateStore::new(config.clone())
        .cleanup(&pool, 3)
        .await
        .unwrap();

    assert_eq!(count(&pool, "logs").await, 3);
    assert_eq!(report.logs, 7, "the report says how many lines were pruned");
    let rows = db::recent_logs(&pool, 10, db::LogSort::Message, db::LogOrder::Asc, None)
        .await
        .unwrap();
    assert!(
        rows.iter().any(|row| row.message == "line 9"),
        "the newest lines are the ones kept"
    );
    assert!(
        !rows.iter().any(|row| row.message == "line 0"),
        "the oldest lines are the ones pruned"
    );
}

/// Stale hosts, robots.txt entries, context entries and recently checked
/// users are expired by the same rules a run's load applies; fresh ones
/// stay, and seen_urls and known_followings are never touched.
#[tokio::test]
async fn cleanup_expires_stale_state_by_the_load_rules() {
    let dir = common::tmp_dir();
    let config = common::test_config(&dir, "example.social");
    let pool = seeded_db(&config).await;

    for (host, checked) in [("old.example", old_ts()), ("fresh.example", now_ts())] {
        sqlx::query(
            "INSERT INTO seen_hosts (host, webserver, software, version, mastodon_api,
             misskey_api, lemmy_api, peertube_api, last_checked, error, raw_nodeinfo)
             VALUES (?1, ?1, 'mastodon', '4', 1, 0, 0, 0, ?2, NULL, '{}')",
        )
        .bind(host)
        .bind(&checked)
        .execute(&pool)
        .await
        .unwrap();
    }
    for (url, fetched) in [("old.txt", old_ts()), ("fresh.txt", now_ts())] {
        sqlx::query("INSERT INTO robots_cache (url, text, fetched_at) VALUES (?1, '', ?2)")
            .bind(url)
            .bind(fetched)
            .execute(&pool)
            .await
            .unwrap();
    }
    for (uri, seen) in [("old-ctx", old_ts()), ("fresh-ctx", now_ts())] {
        sqlx::query("INSERT INTO context_cache (uri, last_seen, created_at) VALUES (?1, ?2, ?2)")
            .bind(uri)
            .bind(seen)
            .execute(&pool)
            .await
            .unwrap();
    }
    for (acct, seen) in [("old-user", old_ts()), ("fresh-user", now_ts())] {
        sqlx::query("INSERT INTO recently_checked_users (acct, first_seen) VALUES (?1, ?2)")
            .bind(acct)
            .bind(seen)
            .execute(&pool)
            .await
            .unwrap();
    }

    let report = StateStore::new(config.clone())
        .cleanup(&pool, 100_000)
        .await
        .unwrap();

    assert_eq!(count(&pool, "seen_hosts").await, 1, "the fresh host stays");
    let hosts: Vec<String> = sqlx::query("SELECT host FROM seen_hosts")
        .fetch_all(&pool)
        .await
        .unwrap()
        .iter()
        .map(|row| row.get::<String, _>("host"))
        .collect();
    assert!(hosts.contains(&"fresh.example".to_string()));
    assert_eq!(report.hosts, 1, "the stale host is expired");

    assert_eq!(
        count(&pool, "robots_cache").await,
        1,
        "the fresh entry stays"
    );
    assert_eq!(report.robots, 1);

    let ctx: Vec<String> = sqlx::query("SELECT uri FROM context_cache")
        .fetch_all(&pool)
        .await
        .unwrap()
        .iter()
        .map(|row| row.get::<String, _>("uri"))
        .collect();
    assert_eq!(ctx, vec!["fresh-ctx".to_string()]);
    assert_eq!(report.context, 1);

    let users: Vec<String> = sqlx::query("SELECT acct FROM recently_checked_users")
        .fetch_all(&pool)
        .await
        .unwrap()
        .iter()
        .map(|row| row.get::<String, _>("acct"))
        .collect();
    assert_eq!(users, vec!["fresh-user".to_string()]);
    assert_eq!(report.users, 1);

    assert_eq!(
        count(&pool, "seen_urls").await,
        1,
        "seen_urls has no expiry"
    );
    assert_eq!(
        count(&pool, "known_followings").await,
        1,
        "known_followings has no expiry"
    );
}

/// The configuration, the OAuth rows, the run history and the accounts'
/// recorded wishes survive a cleanup untouched.
#[tokio::test]
async fn cleanup_keeps_the_records_that_matter() {
    let dir = common::tmp_dir();
    let config = common::test_config(&dir, "example.social");
    let pool = seeded_db(&config).await;

    StateStore::new(config.clone())
        .cleanup(&pool, 0)
        .await
        .unwrap();

    assert_eq!(count(&pool, "config").await, 1);
    assert_eq!(count(&pool, "oauth_accounts").await, 1);
    assert_eq!(count(&pool, "oauth_apps").await, 1);
    assert_eq!(count(&pool, "runs").await, 1);
    assert_eq!(count(&pool, "opted_out_accounts").await, 1);
    assert_eq!(count(&pool, "private_posts").await, 1);
    assert_eq!(count(&pool, "trigger_posts").await, 1);
    assert_eq!(count(&pool, "known_context_posts").await, 1);
    assert_eq!(count(&pool, "replied_toots").await, 1);

    let server: String = sqlx::query("SELECT value FROM config WHERE key = 'server'")
        .fetch_one(&pool)
        .await
        .unwrap()
        .get("value");
    assert_eq!(server, "example.social");
}
