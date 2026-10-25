//! The SQLite database in the state directory: the configuration, the log
//! lines, the web UI's run history, what FediFetcher remembers between runs
//! (the posts and accounts it has seen, and the hosts it has learned
//! about), what earlier runs learned — the posts found private and the
//! accounts found opted out — and the trigger posts a run scanned for
//! context. One file (`fedifetcher.db`) that the web UI can query and later
//! runs consult.

use serde_json::Value;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePool, SqlitePoolOptions};
use sqlx::{AssertSqlSafe, Row};
use time::OffsetDateTime;
use tracing::debug;

use crate::config::{Config, RawValues};
use crate::error::{Error, Result};
use crate::posts::Post;
use crate::servers::HostRow;
use crate::state::TimestampedSet;
use crate::store::{RepliedToot, State};
use crate::timeutil;
use crate::urls::PostRef;

/// One log line, in the pieces the web UI's /logs page needs to render it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LogRow {
    /// The local time, the way `log_format`'s asctime spells it.
    pub ts: String,
    /// DEBUG, INFO, WARNING or ERROR.
    pub level: String,
    /// The module the line came from, e.g. fedifetcher::http.
    pub target: String,
    pub message: String,
}

/// One run of FediFetcher, as the dashboard's history shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunRow {
    pub started: String,
    pub finished: String,
    /// "ok", "stopped" or "error".
    pub outcome: String,
    pub message: String,
}

/// How long a write waits for another connection's transaction to finish
/// before failing with "database is locked". A state save can hold the
/// database's single write lock for seconds; a write that arrives meanwhile
/// waits it out rather than failing.
const BUSY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// Open the database, creating it and its tables if they are not there yet.
pub async fn open(config: &Config) -> Result<SqlitePool> {
    std::fs::create_dir_all(&config.state_dir).map_err(|ex| Error::Io {
        context: format!("Could not create {}", config.state_dir.display()),
        source: ex,
    })?;
    let path = config.state_dir.join("fedifetcher.db");
    let pool = SqlitePoolOptions::new()
        .max_connections(5)
        .connect_with(sqlite_options(&path, BUSY_TIMEOUT))
        .await
        .map_err(|ex| Error::DbOpen { path, source: ex })?;
    create_schema(&pool).await?;
    Ok(pool)
}

/// The options every pool to the database file shares: WAL, so the web UI
/// can read while a run is writing, and a busy timeout, so a write waits
/// out another connection's transaction instead of failing at once.
fn sqlite_options(
    path: &std::path::Path,
    busy_timeout: std::time::Duration,
) -> SqliteConnectOptions {
    SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal)
        .busy_timeout(busy_timeout)
}

/// Create the tables if they are not there yet, and reshape the ones older
/// versions wrote.
async fn create_schema(pool: &SqlitePool) -> Result<()> {
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS logs (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            ts TEXT NOT NULL,
            level TEXT NOT NULL,
            target TEXT NOT NULL,
            message TEXT NOT NULL
        )",
    )
    .execute(pool)
    .await?;
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS runs (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            started TEXT NOT NULL,
            finished TEXT NOT NULL,
            outcome TEXT NOT NULL,
            message TEXT NOT NULL
        )",
    )
    .execute(pool)
    .await?;
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS private_posts (
            uri TEXT PRIMARY KEY,
            first_marked TEXT NOT NULL
        )",
    )
    .execute(pool)
    .await?;
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS opted_out_accounts (
            acct TEXT PRIMARY KEY,
            reason TEXT NOT NULL,
            first_seen TEXT NOT NULL
        )",
    )
    .execute(pool)
    .await?;
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS known_context_posts (
            uri TEXT PRIMARY KEY,
            first_seen TEXT NOT NULL,
            last_seen TEXT NOT NULL
        )",
    )
    .execute(pool)
    .await?;
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS trigger_posts (
            uri TEXT PRIMARY KEY,
            url TEXT NOT NULL,
            acct TEXT,
            replies_count INTEGER,
            created_at TEXT,
            source TEXT NOT NULL,
            first_seen TEXT NOT NULL,
            last_seen TEXT NOT NULL,
            in_reply_to_id TEXT,
            in_reply_to_account_id TEXT,
            is_public INTEGER NOT NULL DEFAULT 1,
            is_boost INTEGER NOT NULL DEFAULT 0,
            author_url TEXT,
            mentions TEXT
        )",
    )
    .execute(pool)
    .await?;
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS config (
            key TEXT PRIMARY KEY,
            value TEXT NOT NULL
        )",
    )
    .execute(pool)
    .await?;
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS oauth_accounts (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            token TEXT NOT NULL UNIQUE,
            server TEXT NOT NULL,
            acct TEXT NOT NULL,
            avatar TEXT NOT NULL,
            connected_at TEXT NOT NULL,
            needs_reconnect INTEGER NOT NULL DEFAULT 0
        )",
    )
    .execute(pool)
    .await?;
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS oauth_apps (
            server TEXT PRIMARY KEY,
            client_id TEXT NOT NULL,
            client_secret TEXT NOT NULL,
            scopes TEXT NOT NULL DEFAULT '',
            registered_at TEXT NOT NULL
        )",
    )
    .execute(pool)
    .await?;
    migrate_oauth_apps(pool).await?;
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS seen_urls (
            uri TEXT PRIMARY KEY,
            first_seen TEXT NOT NULL
        )",
    )
    .execute(pool)
    .await?;
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS known_followings (
            acct TEXT PRIMARY KEY,
            first_seen TEXT NOT NULL
        )",
    )
    .execute(pool)
    .await?;
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS recently_checked_users (
            acct TEXT PRIMARY KEY,
            first_seen TEXT NOT NULL
        )",
    )
    .execute(pool)
    .await?;
    create_seen_hosts(pool).await?;
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS context_cache (
            uri TEXT PRIMARY KEY,
            last_seen TEXT NOT NULL,
            created_at TEXT NOT NULL
        )",
    )
    .execute(pool)
    .await?;
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS replied_toots (
            url TEXT PRIMARY KEY,
            remote_url TEXT,
            server TEXT,
            post_id TEXT
        )",
    )
    .execute(pool)
    .await?;
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS robots_cache (
            url TEXT PRIMARY KEY,
            text TEXT NOT NULL,
            fetched_at TEXT NOT NULL
        )",
    )
    .execute(pool)
    .await?;
    // databases written by earlier versions get their tables reshaped here
    migrate_trigger_posts(pool).await?;
    migrate_seen_hosts(pool).await?;
    Ok(())
}

/// Create the seen_hosts table, whose fields are columns rather than one
/// JSON blob.
async fn create_seen_hosts(pool: &SqlitePool) -> Result<()> {
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS seen_hosts (
            host TEXT PRIMARY KEY,
            webserver TEXT,
            software TEXT,
            version TEXT,
            mastodon_api INTEGER NOT NULL DEFAULT 0,
            misskey_api INTEGER NOT NULL DEFAULT 0,
            lemmy_api INTEGER NOT NULL DEFAULT 0,
            peertube_api INTEGER NOT NULL DEFAULT 0,
            last_checked TEXT,
            error TEXT,
            raw_nodeinfo TEXT
        )",
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// Add the columns an older trigger_posts table is missing, keeping the rows
/// it already has (the new columns read as a public, non-boost post).
/// The oauth_apps table of an earlier build knows nothing of the scopes
/// its registration asked for — which is how a stale registration is
/// recognized.
async fn migrate_oauth_apps(pool: &SqlitePool) -> Result<()> {
    let columns = table_columns(pool, "oauth_apps").await?;
    if !columns.iter().any(|name| name == "scopes") {
        sqlx::query("ALTER TABLE oauth_apps ADD COLUMN scopes TEXT NOT NULL DEFAULT ''")
            .execute(pool)
            .await?;
    }
    Ok(())
}

async fn migrate_trigger_posts(pool: &SqlitePool) -> Result<()> {
    let columns = table_columns(pool, "trigger_posts").await?;
    let missing = [
        ("in_reply_to_id", "TEXT"),
        ("in_reply_to_account_id", "TEXT"),
        ("is_public", "INTEGER NOT NULL DEFAULT 1"),
        ("is_boost", "INTEGER NOT NULL DEFAULT 0"),
        ("author_url", "TEXT"),
        ("mentions", "TEXT"),
    ];
    for (column, kind) in missing {
        if !columns.iter().any(|name| name == column) {
            // the column and kind come from the hardcoded list above
            let sql = format!("ALTER TABLE trigger_posts ADD COLUMN {column} {kind}");
            sqlx::query(AssertSqlSafe(sql)).execute(pool).await?;
        }
    }
    Ok(())
}

/// How many times a write is retried while another connection holds the
/// database's single write lock. Each attempt already waits out the busy
/// timeout; the retries cover a lock held longer still, and the snapshot
/// conflict below, which no timeout waits out.
const LOCKED_RETRIES: usize = 3;

/// Whether a database error is the write lock being held by another
/// connection: SQLITE_BUSY (5), or SQLITE_BUSY_SNAPSHOT (517) — a
/// transaction that read, then found another connection had written since,
/// so its read snapshot can no longer be upgraded to a write. The busy
/// timeout waits out the first; the second fails at once. Both are worth
/// retrying: a fresh attempt gets a fresh snapshot and a fresh chance at
/// the lock.
fn is_locked(ex: &sqlx::Error) -> bool {
    match ex {
        sqlx::Error::Database(db) => {
            matches!(db.code().as_deref(), Some("5") | Some("517"))
        }
        _ => false,
    }
}

/// Run a database write, retrying while another connection holds the
/// database's single write lock. SQLite lets one connection write at a
/// time; a write that arrives while a long transaction — a state save, a
/// scan's trigger-post record — is running fails at once with "database is
/// locked" rather than queueing, so the write is tried again after a short
/// pause. The write must be safe to repeat: a failed attempt's transaction
/// rolls back when it is dropped.
async fn retry_locked<T, F, Fut>(mut write: F) -> Result<T>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = std::result::Result<T, sqlx::Error>>,
{
    let mut attempt = 0;
    loop {
        match write().await {
            Ok(value) => return Ok(value),
            Err(ex) if is_locked(&ex) && attempt < LOCKED_RETRIES => {
                attempt += 1;
                debug!("Database is locked; retrying the write (attempt {attempt})");
                tokio::time::sleep(std::time::Duration::from_millis(50 * attempt as u64)).await;
            }
            Err(ex) => return Err(ex.into()),
        }
    }
}

/// Rebuild a seen_hosts table that kept every field in one JSON `info`
/// column, breaking each row's fields out into columns. Rows whose JSON does
/// not parse are dropped, as the load path dropped them.
async fn migrate_seen_hosts(pool: &SqlitePool) -> Result<()> {
    let columns = table_columns(pool, "seen_hosts").await?;
    if !columns.iter().any(|name| name == "info") {
        return Ok(());
    }
    let rows = sqlx::query("SELECT host, info FROM seen_hosts ORDER BY rowid")
        .fetch_all(pool)
        .await?;
    sqlx::query("ALTER TABLE seen_hosts RENAME TO seen_hosts_legacy")
        .execute(pool)
        .await?;
    create_seen_hosts(pool).await?;
    for row in rows {
        let host: String = row.get("host");
        let info: String = row.get("info");
        let Ok(value) = serde_json::from_str::<Value>(&info) else {
            continue;
        };
        insert_host_row(pool, &HostRow::from_state_value(&host, &value)).await?;
    }
    sqlx::query("DROP TABLE seen_hosts_legacy")
        .execute(pool)
        .await?;
    Ok(())
}

/// The stored configuration as raw values: the database's whole `config`
/// table. An empty table reads as an empty set.
pub async fn load_config(pool: &SqlitePool) -> Result<RawValues> {
    let rows = sqlx::query("SELECT key, value FROM config ORDER BY rowid")
        .fetch_all(pool)
        .await?;
    let mut values = RawValues::new();
    for row in rows {
        let key: String = row.get("key");
        let text: String = row.get("value");
        let value: Value = serde_json::from_str(&text)
            .map_err(|ex| Error::Config(format!("Cannot read the stored option {key}: {ex}")))?;
        values.insert(key, value);
    }
    Ok(values)
}

/// Replace the stored configuration: options absent from `values` are
/// cleared, so an unset option stays unset when the config is read back.
/// The app registered on a server, if it is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OauthAppRow {
    pub server: String,
    pub client_id: String,
    pub client_secret: String,
    pub scopes: String,
}

/// The app registered on the given server, if it is.
pub async fn oauth_app(pool: &SqlitePool, server: &str) -> Result<Option<OauthAppRow>> {
    let row = sqlx::query(
        "SELECT server, client_id, client_secret, scopes FROM oauth_apps WHERE server = ?",
    )
    .bind(server)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|row| OauthAppRow {
        server: row.get("server"),
        client_id: row.get("client_id"),
        client_secret: row.get("client_secret"),
        scopes: row.get("scopes"),
    }))
}

/// Remember the app registration for a server, replacing any earlier one.
pub async fn save_oauth_app(
    pool: &SqlitePool,
    server: &str,
    client_id: &str,
    client_secret: &str,
    scopes: &str,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO oauth_apps (server, client_id, client_secret, scopes, registered_at)
         VALUES (?, ?, ?, ?, ?)
         ON CONFLICT(server) DO UPDATE SET client_id = excluded.client_id,
         client_secret = excluded.client_secret, scopes = excluded.scopes,
         registered_at = excluded.registered_at",
    )
    .bind(server)
    .bind(client_id)
    .bind(client_secret)
    .bind(scopes)
    .bind(crate::timeutil::fmt_state_ts(crate::timeutil::now()))
    .execute(pool)
    .await?;
    Ok(())
}

/// A connected account, as the OAuth flow and the runs leave it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OauthAccountRow {
    pub token: String,
    pub server: String,
    pub acct: String,
    pub avatar: String,
    pub connected_at: String,
    pub needs_reconnect: bool,
}

/// The connected account a token belongs to, if it was connected by OAuth.
pub async fn oauth_account(pool: &SqlitePool, token: &str) -> Result<Option<OauthAccountRow>> {
    let row = sqlx::query(
        "SELECT token, server, acct, avatar, connected_at, needs_reconnect
         FROM oauth_accounts WHERE token = ?",
    )
    .bind(token)
    .fetch_optional(pool)
    .await?;
    Ok(row.as_ref().map(connected_account_row))
}

/// Every connected account, oldest first.
pub async fn oauth_accounts(pool: &SqlitePool) -> Result<Vec<OauthAccountRow>> {
    let rows = sqlx::query(
        "SELECT token, server, acct, avatar, connected_at, needs_reconnect
         FROM oauth_accounts ORDER BY id",
    )
    .fetch_all(pool)
    .await?;
    Ok(rows.iter().map(connected_account_row).collect())
}

/// The connected accounts for one account on one server: the same account
/// may hold more than one token.
pub async fn oauth_accounts_by_acct(
    pool: &SqlitePool,
    acct: &str,
    server: &str,
) -> Result<Vec<OauthAccountRow>> {
    let rows = sqlx::query(
        "SELECT token, server, acct, avatar, connected_at, needs_reconnect
         FROM oauth_accounts WHERE acct = ? AND server = ? ORDER BY id",
    )
    .bind(acct)
    .bind(server)
    .fetch_all(pool)
    .await?;
    Ok(rows.iter().map(connected_account_row).collect())
}

/// Mark (or clear) that an instance rejected a connected account's token:
/// the run loop's 401s speak here, the config page's row renders it.
pub async fn mark_oauth_needs_reconnect(pool: &SqlitePool, token: &str, needs: bool) -> Result<()> {
    sqlx::query("UPDATE oauth_accounts SET needs_reconnect = ? WHERE token = ?")
        .bind(needs)
        .bind(token)
        .execute(pool)
        .await?;
    Ok(())
}

/// Forget a connected account's row; its token is being removed.
pub async fn delete_oauth_account(pool: &SqlitePool, token: &str) -> Result<()> {
    sqlx::query("DELETE FROM oauth_accounts WHERE token = ?")
        .bind(token)
        .execute(pool)
        .await?;
    Ok(())
}

/// Forget the rows of several removed tokens, the config form's index
/// checkboxes having spoken.
pub async fn delete_oauth_accounts(pool: &SqlitePool, tokens: &[String]) -> Result<()> {
    for token in tokens {
        delete_oauth_account(pool, token).await?;
    }
    Ok(())
}

fn connected_account_row(row: &sqlx::sqlite::SqliteRow) -> OauthAccountRow {
    OauthAccountRow {
        token: row.get("token"),
        server: row.get("server"),
        acct: row.get("acct"),
        avatar: row.get("avatar"),
        connected_at: row.get("connected_at"),
        needs_reconnect: row.get::<i64, _>("needs_reconnect") != 0,
    }
}

/// Record a freshly connected account.
pub async fn save_oauth_account(
    pool: &SqlitePool,
    token: &str,
    server: &str,
    acct: &str,
    avatar: &str,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO oauth_accounts (token, server, acct, avatar, connected_at, needs_reconnect)
         VALUES (?, ?, ?, ?, ?, 0)
         ON CONFLICT(token) DO UPDATE SET acct = excluded.acct,
         avatar = excluded.avatar, needs_reconnect = 0",
    )
    .bind(token)
    .bind(server)
    .bind(acct)
    .bind(avatar)
    .bind(crate::timeutil::fmt_state_ts(crate::timeutil::now()))
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn save_config(pool: &SqlitePool, values: &RawValues) -> Result<()> {
    let mut tx = pool.begin().await?;
    sqlx::query("DELETE FROM config").execute(&mut *tx).await?;
    for (key, value) in values {
        sqlx::query("INSERT INTO config (key, value) VALUES (?, ?)")
            .bind(key)
            .bind(value.to_string())
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    Ok(())
}

/// The tables a cleanup keeps: the configuration's records, and the OAuth
/// rows — the only data whose loss is not repaired by fetching again.
const KEPT_TABLES: &[&str] = &["config", "oauth_accounts", "oauth_apps"];

/// The tables a cleanup empties, with the singular label the report shows
/// them by.
const CLEANED_TABLES: &[(&str, &str)] = &[
    ("logs", "log line"),
    ("runs", "run"),
    ("seen_urls", "seen URL"),
    ("known_followings", "known following"),
    ("recently_checked_users", "recently checked user"),
    ("seen_hosts", "known host"),
    ("context_cache", "context entry"),
    ("replied_toots", "replied-toot record"),
    ("robots_cache", "robots.txt entry"),
    ("trigger_posts", "scanned post"),
    ("known_context_posts", "known context post"),
    ("private_posts", "private-post marker"),
    ("opted_out_accounts", "opted-out record"),
];

/// What a cleanup removed: one entry per table that had rows to remove,
/// label first.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CleanupReport {
    pub removed: Vec<(&'static str, u64)>,
}

/// How much of each table a trim takes: the oldest tenth, rounded down.
const TRIM_FRACTION_NUMERATOR: i64 = 1;
const TRIM_FRACTION_DENOMINATOR: i64 = 10;

/// Empty every table but the kept ones, then vacuum, so the freed space
/// goes back to the filesystem. FediFetcher re-fetches what it needs on
/// its next run; a cleanup never costs the configuration, the OAuth rows,
/// or anything else that is not re-fetchable.
pub async fn cleanup(pool: &SqlitePool) -> Result<CleanupReport> {
    for table in KEPT_TABLES {
        debug_assert!(
            !CLEANED_TABLES.iter().any(|(name, _)| name == table),
            "{table} is both kept and cleaned"
        );
    }
    let mut removed = Vec::new();
    // one static DELETE per table: sqlx audits dynamic SQL strings
    for (sql, label) in [
        ("DELETE FROM logs", "log line"),
        ("DELETE FROM runs", "run"),
        ("DELETE FROM seen_urls", "seen URL"),
        ("DELETE FROM known_followings", "known following"),
        (
            "DELETE FROM recently_checked_users",
            "recently checked user",
        ),
        ("DELETE FROM seen_hosts", "known host"),
        ("DELETE FROM context_cache", "context entry"),
        ("DELETE FROM replied_toots", "replied-toot record"),
        ("DELETE FROM robots_cache", "robots.txt entry"),
        ("DELETE FROM trigger_posts", "scanned post"),
        ("DELETE FROM known_context_posts", "known context post"),
        ("DELETE FROM private_posts", "private-post marker"),
        ("DELETE FROM opted_out_accounts", "opted-out record"),
    ] {
        let result = sqlx::query(sql).execute(pool).await?;
        if result.rows_affected() > 0 {
            removed.push((label, result.rows_affected()));
        }
    }
    sqlx::raw_sql("VACUUM; PRAGMA wal_checkpoint(TRUNCATE);")
        .execute(pool)
        .await?;
    Ok(CleanupReport { removed })
}

/// Remove the oldest tenth of each cleanable table — the same tables a
/// cleanup empties, but only their oldest tenth of rows, so the
/// newest records (and the kept tables) stay. Rows whose age column is
/// unreadable count as the oldest.
pub async fn trim(pool: &SqlitePool) -> Result<CleanupReport> {
    let mut removed = Vec::new();
    // one static pair per table — a count, and a delete of the oldest
    // rows by that table's age column (rowid where it has none) — because
    // sqlx audits dynamic SQL strings
    for (count_sql, delete_sql, label) in [
        (
            "SELECT COUNT(*) AS n FROM logs",
            "DELETE FROM logs WHERE id IN (SELECT id FROM logs ORDER BY id LIMIT ?1)",
            "log line",
        ),
        (
            "SELECT COUNT(*) AS n FROM runs",
            "DELETE FROM runs WHERE id IN (SELECT id FROM runs ORDER BY id LIMIT ?1)",
            "run",
        ),
        (
            "SELECT COUNT(*) AS n FROM seen_urls",
            "DELETE FROM seen_urls WHERE uri IN (SELECT uri FROM seen_urls ORDER BY first_seen LIMIT ?1)",
            "seen URL",
        ),
        (
            "SELECT COUNT(*) AS n FROM known_followings",
            "DELETE FROM known_followings WHERE acct IN (SELECT acct FROM known_followings ORDER BY first_seen LIMIT ?1)",
            "known following",
        ),
        (
            "SELECT COUNT(*) AS n FROM recently_checked_users",
            "DELETE FROM recently_checked_users WHERE acct IN (SELECT acct FROM recently_checked_users ORDER BY first_seen LIMIT ?1)",
            "recently checked user",
        ),
        (
            "SELECT COUNT(*) AS n FROM seen_hosts",
            "DELETE FROM seen_hosts WHERE host IN (SELECT host FROM seen_hosts ORDER BY last_checked LIMIT ?1)",
            "known host",
        ),
        (
            "SELECT COUNT(*) AS n FROM context_cache",
            "DELETE FROM context_cache WHERE uri IN (SELECT uri FROM context_cache ORDER BY last_seen LIMIT ?1)",
            "context entry",
        ),
        (
            "SELECT COUNT(*) AS n FROM replied_toots",
            "DELETE FROM replied_toots WHERE rowid IN (SELECT rowid FROM replied_toots ORDER BY rowid LIMIT ?1)",
            "replied-toot record",
        ),
        (
            "SELECT COUNT(*) AS n FROM robots_cache",
            "DELETE FROM robots_cache WHERE url IN (SELECT url FROM robots_cache ORDER BY fetched_at LIMIT ?1)",
            "robots.txt entry",
        ),
        (
            "SELECT COUNT(*) AS n FROM trigger_posts",
            "DELETE FROM trigger_posts WHERE uri IN (SELECT uri FROM trigger_posts ORDER BY first_seen LIMIT ?1)",
            "scanned post",
        ),
        (
            "SELECT COUNT(*) AS n FROM known_context_posts",
            "DELETE FROM known_context_posts WHERE uri IN (SELECT uri FROM known_context_posts ORDER BY first_seen LIMIT ?1)",
            "known context post",
        ),
        (
            "SELECT COUNT(*) AS n FROM private_posts",
            "DELETE FROM private_posts WHERE uri IN (SELECT uri FROM private_posts ORDER BY first_marked LIMIT ?1)",
            "private-post marker",
        ),
        (
            "SELECT COUNT(*) AS n FROM opted_out_accounts",
            "DELETE FROM opted_out_accounts WHERE acct IN (SELECT acct FROM opted_out_accounts ORDER BY first_seen LIMIT ?1)",
            "opted-out record",
        ),
    ] {
        let total: i64 = sqlx::query_scalar(count_sql).fetch_one(pool).await?;
        let tenth = total * TRIM_FRACTION_NUMERATOR / TRIM_FRACTION_DENOMINATOR;
        if tenth <= 0 {
            continue;
        }
        let result = sqlx::query(delete_sql).bind(tenth).execute(pool).await?;
        if result.rows_affected() > 0 {
            removed.push((label, result.rows_affected()));
        }
    }
    sqlx::raw_sql("VACUUM; PRAGMA wal_checkpoint(TRUNCATE);")
        .execute(pool)
        .await?;
    Ok(CleanupReport { removed })
}

/// Add one log line.
pub async fn insert_log(pool: &SqlitePool, row: &LogRow) -> Result<()> {
    sqlx::query("INSERT INTO logs (ts, level, target, message) VALUES (?, ?, ?, ?)")
        .bind(&row.ts)
        .bind(&row.level)
        .bind(&row.target)
        .bind(&row.message)
        .execute(pool)
        .await?;
    Ok(())
}

/// Which column of the log table the /logs page sorts by.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LogSort {
    Time,
    Level,
    Message,
}

impl LogSort {
    /// The /logs query string's name for the column.
    pub fn param(self) -> &'static str {
        match self {
            LogSort::Time => "ts",
            LogSort::Level => "level",
            LogSort::Message => "message",
        }
    }

    /// The column a /logs query string asks for, defaulting to time.
    pub fn parse(param: Option<&str>) -> Self {
        match param {
            Some("level") => LogSort::Level,
            Some("message") => LogSort::Message,
            _ => LogSort::Time,
        }
    }
}

/// The direction the /logs page sorts the log table in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LogOrder {
    Asc,
    Desc,
}

impl LogOrder {
    /// The query string's name for the direction.
    pub fn param(self) -> &'static str {
        match self {
            LogOrder::Asc => "asc",
            LogOrder::Desc => "desc",
        }
    }
}

/// The most recent log lines, sorted as the /logs page asks. The newest
/// `max` lines are fetched, then that page is sorted by `sort`; lines equal
/// on the sorted column keep their newest-first order.
///
/// `levels` narrows the lines to those levels before the `max` is applied:
/// `None` shows every level, and `Some(&[])` shows none.
pub async fn recent_logs(
    pool: &SqlitePool,
    max: usize,
    sort: LogSort,
    order: LogOrder,
    levels: Option<&[String]>,
) -> Result<Vec<LogRow>> {
    let rows = match levels {
        Some([]) => return Ok(Vec::new()),
        Some(levels) => {
            let mut query = sqlx::QueryBuilder::new(
                "SELECT ts, level, target, message FROM logs WHERE level IN (",
            );
            let mut separated = query.separated(", ");
            for level in levels {
                separated.push_bind(level);
            }
            separated.push_unseparated(") ORDER BY id DESC LIMIT ");
            query.push_bind(max as i64);
            query.build().fetch_all(pool).await?
        }
        None => {
            sqlx::query("SELECT ts, level, target, message FROM logs ORDER BY id DESC LIMIT ?")
                .bind(max as i64)
                .fetch_all(pool)
                .await?
        }
    };
    // newest first
    let mut lines: Vec<LogRow> = rows
        .iter()
        .map(|row| LogRow {
            ts: row.get("ts"),
            level: row.get("level"),
            target: row.get("target"),
            message: row.get("message"),
        })
        .collect();
    if sort != LogSort::Time {
        // a stable sort, so equal keys keep the newest-first order above
        lines.sort_by(|a, b| match sort {
            LogSort::Level => a.level.cmp(&b.level),
            LogSort::Message => a.message.cmp(&b.message),
            LogSort::Time => std::cmp::Ordering::Equal,
        });
    }
    match (sort, order) {
        // time order is already newest first
        (LogSort::Time, LogOrder::Desc) => {}
        (LogSort::Time, LogOrder::Asc) => lines.reverse(),
        // the sort above is ascending; flip it for descending
        (_, LogOrder::Asc) => {}
        (_, LogOrder::Desc) => lines.reverse(),
    }
    Ok(lines)
}

/// Delete every log line; the /logs page's clear button. Lines already on
/// stderr stay there; the database is what the page reads.
pub async fn clear_logs(pool: &SqlitePool) -> Result<()> {
    sqlx::query("DELETE FROM logs").execute(pool).await?;
    Ok(())
}

/// Keep only the newest `keep` log lines.
pub async fn prune_logs(pool: &SqlitePool, keep: i64) -> Result<()> {
    sqlx::query("DELETE FROM logs WHERE id <= (SELECT COALESCE(MAX(id), 0) - ? FROM logs)")
        .bind(keep)
        .execute(pool)
        .await?;
    Ok(())
}

/// Record one finished run.
pub async fn insert_run(pool: &SqlitePool, run: &RunRow) -> Result<()> {
    sqlx::query("INSERT INTO runs (started, finished, outcome, message) VALUES (?, ?, ?, ?)")
        .bind(&run.started)
        .bind(&run.finished)
        .bind(&run.outcome)
        .bind(&run.message)
        .execute(pool)
        .await?;
    Ok(())
}

/// Record a run the moment it starts, so the dashboard's history shows it
/// while it is still going: the row reads `running`, with no end time, until
/// `finish_run` completes it. Returns the row's id.
pub async fn start_run(pool: &SqlitePool, started: &str) -> Result<i64> {
    let result = sqlx::query(
        "INSERT INTO runs (started, finished, outcome, message) VALUES (?, '', 'running', '')",
    )
    .bind(started)
    .execute(pool)
    .await?;
    Ok(result.last_insert_rowid())
}

/// Complete the run `start_run` recorded: its end time, its outcome, and
/// the message it finished with.
pub async fn finish_run(
    pool: &SqlitePool,
    id: i64,
    finished: &str,
    outcome: &str,
    message: &str,
) -> Result<()> {
    sqlx::query("UPDATE runs SET finished = ?, outcome = ?, message = ? WHERE id = ?")
        .bind(finished)
        .bind(outcome)
        .bind(message)
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

/// Mark runs still recorded as running as interrupted. The web UI calls this
/// when it starts, under the lock: a `running` row it finds then belongs to
/// a process that is gone, and would otherwise read as running forever. The
/// message says how long the run had been going, from its start to `now`.
pub async fn interrupt_stale_runs(pool: &SqlitePool, now: OffsetDateTime) -> Result<u64> {
    let rows = sqlx::query("SELECT id, started FROM runs WHERE outcome = 'running'")
        .fetch_all(pool)
        .await?;
    for row in &rows {
        let message = match timeutil::parse(&row.get::<String, _>("started")) {
            Some(started) => format!(
                "Interrupted: FediFetcher stopped while this run was in progress (running for {})",
                timeutil::fmt_timedelta(now - started)
            ),
            None => "Interrupted: FediFetcher stopped while this run was in progress".to_string(),
        };
        sqlx::query(
            "UPDATE runs SET outcome = 'error', message = ?, finished = started WHERE id = ?",
        )
        .bind(message)
        .bind(row.get::<i64, _>("id"))
        .execute(pool)
        .await?;
    }
    Ok(rows.len() as u64)
}

/// The most recent runs, newest first.
pub async fn recent_runs(pool: &SqlitePool, max: usize) -> Result<Vec<RunRow>> {
    let rows = sqlx::query(
        "SELECT started, finished, outcome, message FROM runs ORDER BY id DESC LIMIT ?",
    )
    .bind(max as i64)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .iter()
        .map(|row| RunRow {
            started: row.get("started"),
            finished: row.get("finished"),
            outcome: row.get("outcome"),
            message: row.get("message"),
        })
        .collect())
}

/// The tables in the database, for the data page's sub-tabs. SQLite's
/// internal tables (`sqlite_sequence` and friends) are left out.
pub async fn list_tables(pool: &SqlitePool) -> Result<Vec<String>> {
    let rows = sqlx::query(
        "SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%' ORDER BY name",
    )
    .fetch_all(pool)
    .await
    ?;
    Ok(rows
        .iter()
        .map(|row| row.get::<String, _>("name"))
        .collect())
}

/// One table's columns, in declaration order, for the data page's table
/// view.
pub async fn table_columns(pool: &SqlitePool, table: &str) -> Result<Vec<String>> {
    let sql = format!("PRAGMA table_info({})", quote_ident(table)?);
    let rows = sqlx::query(AssertSqlSafe(sql)).fetch_all(pool).await?;
    Ok(rows
        .iter()
        .map(|row| row.get::<String, _>("name"))
        .collect())
}

/// How many rows a table has.
pub async fn table_count(pool: &SqlitePool, table: &str) -> Result<i64> {
    let sql = format!("SELECT COUNT(*) FROM {}", quote_ident(table)?);
    let count: i64 = sqlx::query_scalar(AssertSqlSafe(sql))
        .fetch_one(pool)
        .await?;
    Ok(count)
}

/// The first `max` rows of a table, sorted by the `sort` column, every value
/// as text (`None` for SQL NULL). `columns` and `sort` must be real names
/// from `table_columns`; anything else is rejected rather than interpolated.
pub async fn table_rows(
    pool: &SqlitePool,
    table: &str,
    columns: &[String],
    sort: &str,
    order: LogOrder,
    max: i64,
) -> Result<Vec<Vec<Option<String>>>> {
    if columns.is_empty() {
        return Ok(Vec::new());
    }
    if !columns.iter().any(|column| column == sort) {
        return Err(Error::Fetch(format!(
            "{sort:?} is not a column of {table:?}"
        )));
    }
    let table_q = quote_ident(table)?;
    let select: Vec<String> = columns
        .iter()
        .map(|column| {
            let column_q = quote_ident(column)?;
            Ok(format!("CAST({table_q}.{column_q} AS TEXT) AS {column_q}"))
        })
        .collect::<Result<_>>()?;
    let sort_q = quote_ident(sort)?;
    let direction = match order {
        LogOrder::Asc => "ASC",
        LogOrder::Desc => "DESC",
    };
    // ordering by the qualified column keeps the sort on the column's real
    // type, not on the text the select casts it to
    let sql = format!(
        "SELECT {} FROM {table_q} ORDER BY {table_q}.{sort_q} {direction} LIMIT ?",
        select.join(", ")
    );
    let rows = sqlx::query(AssertSqlSafe(sql))
        .bind(max)
        .fetch_all(pool)
        .await?;
    Ok(rows
        .iter()
        .map(|row| {
            columns
                .iter()
                .map(|column| {
                    row.try_get::<Option<String>, _>(column.as_str())
                        .unwrap_or(None)
                })
                .collect()
        })
        .collect())
}

/// Quote a name for interpolation into SQL, rejecting anything that is not
/// a plain identifier. The data page passes names read back from the
/// database itself; this is the guard that keeps the query strings it
/// accepts from reaching the SQL.
fn quote_ident(name: &str) -> Result<String> {
    let safe = !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    if safe {
        Ok(format!("\"{name}\""))
    } else {
        Err(Error::Fetch(format!(
            "{name:?} is not a table or column name"
        )))
    }
}

/// Record a post as private — one whose context can never be fetched — so
/// later runs skip it without resolving its URL again. Returns whether this
/// call is the one that recorded it.
pub async fn mark_private_post(pool: &SqlitePool, uri: &str) -> Result<bool> {
    let result =
        sqlx::query("INSERT OR IGNORE INTO private_posts (uri, first_marked) VALUES (?, ?)")
            .bind(uri)
            .bind(timeutil::fmt_state_ts(timeutil::now()))
            .execute(pool)
            .await?;
    Ok(result.rows_affected() > 0)
}

/// Whether an earlier run recorded this post as private.
pub async fn is_private_post(pool: &SqlitePool, uri: &str) -> Result<bool> {
    let found = sqlx::query("SELECT uri FROM private_posts WHERE uri = ? LIMIT 1")
        .bind(uri)
        .fetch_optional(pool)
        .await?;
    Ok(found.is_some())
}

/// Record an account as opted out, so later runs recognize the skip without
/// logging it again. Returns whether this call is the one that recorded it.
pub async fn mark_opted_out_account(pool: &SqlitePool, acct: &str, reason: &str) -> Result<bool> {
    let result = sqlx::query(
        "INSERT OR IGNORE INTO opted_out_accounts (acct, reason, first_seen) VALUES (?, ?, ?)",
    )
    .bind(acct)
    .bind(reason)
    .bind(timeutil::fmt_state_ts(timeutil::now()))
    .execute(pool)
    .await?;
    Ok(result.rows_affected() > 0)
}

/// The reason an earlier run recorded for this account's opt-out, if any.
pub async fn opted_out_account_reason(pool: &SqlitePool, acct: &str) -> Result<Option<String>> {
    let found = sqlx::query("SELECT reason FROM opted_out_accounts WHERE acct = ? LIMIT 1")
        .bind(acct)
        .fetch_optional(pool)
        .await?;
    Ok(found.map(|row| row.get("reason")))
}

/// Record the context posts a run found, and report what the database
/// already knew about each: `None` when the post is new, or the `last_seen`
/// of the earlier record. `first_seen` is kept from the first record;
/// `last_seen` is refreshed on every sighting.
pub async fn record_known_context_posts(
    pool: &SqlitePool,
    uris: &[String],
) -> Result<Vec<Option<String>>> {
    let now = timeutil::fmt_state_ts(timeutil::now());
    let mut previous = Vec::with_capacity(uris.len());
    for uri in uris {
        let last_seen: Option<String> =
            sqlx::query_scalar("SELECT last_seen FROM known_context_posts WHERE uri = ?")
                .bind(uri)
                .fetch_optional(pool)
                .await?;
        sqlx::query(
            "INSERT INTO known_context_posts (uri, first_seen, last_seen) VALUES (?, ?, ?)
                ON CONFLICT(uri) DO UPDATE SET last_seen = excluded.last_seen",
        )
        .bind(uri)
        .bind(&now)
        .bind(&now)
        .execute(pool)
        .await?;
        previous.push(last_seen);
    }
    Ok(previous)
}

/// How many trigger posts the database keeps; the oldest are pruned as new
/// ones are recorded.
pub const TRIGGER_POSTS_KEPT: i64 = 100_000;

/// A post a run scanned for context — one of the posts that generate the
/// need for backfilling — as the `trigger_posts` table records it, with
/// everything the scan pulled in on it: its links, its visibility, and the
/// accounts it names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TriggerPost {
    /// The post's uri, as its server spells it.
    pub uri: String,
    /// The post's URL, the one a context fetch parses.
    pub url: String,
    /// Who wrote it, `name@host` when the server said.
    pub acct: Option<String>,
    /// The author's profile URL, when the server said.
    pub author_url: Option<String>,
    /// How many replies the server said it had, when it said.
    pub replies_count: Option<i64>,
    /// When the post was made, as a state timestamp.
    pub created_at: Option<String>,
    /// The uri of the post this one replies to, when it is a reply.
    pub in_reply_to_id: Option<String>,
    /// The account id of the post this one replies to, when it is a reply.
    pub in_reply_to_account_id: Option<String>,
    /// Whether the server said the post is public.
    pub is_public: bool,
    /// Whether the post the scan was given was a boost.
    pub is_boost: bool,
    /// The accounts the post mentions, `name@host` each, comma-separated.
    pub mentions: Option<String>,
    /// Which task first scanned it: home timeline, replies, bookmarks,
    /// favorites, lists, backfill, or a manual fetch.
    pub source: String,
}

impl TriggerPost {
    /// The trigger-post record for one of the posts a scan was given. A
    /// boost is recorded as the post it boosts: that is the post whose
    /// context is fetched.
    pub fn from_post(post: &Post, source: &str) -> Self {
        let original = post.original();
        TriggerPost {
            uri: original.uri.clone(),
            url: original.url.clone(),
            acct: original
                .account
                .as_ref()
                .map(|account| account.acct.clone()),
            author_url: original.account.as_ref().map(|account| account.url.clone()),
            replies_count: original.reply_count,
            created_at: Some(timeutil::fmt_state_ts(original.created_at)),
            in_reply_to_id: original.in_reply_to_id.clone(),
            in_reply_to_account_id: original.in_reply_to_account_id.clone(),
            is_public: original.is_public,
            is_boost: post.is_boost(),
            mentions: if original.mentions.is_empty() {
                None
            } else {
                Some(
                    original
                        .mentions
                        .iter()
                        .map(|user| user.acct.clone())
                        .collect::<Vec<_>>()
                        .join(","),
                )
            },
            source: source.to_string(),
        }
    }
}

/// Record the posts a scan was given, refreshing what is known about each:
/// `first_seen` and `source` are kept from the first record; the rest follow
/// the newest sighting. Returns how many of the posts are new, and keeps
/// only the newest `keep` rows.
pub async fn record_trigger_posts(
    pool: &SqlitePool,
    posts: &[TriggerPost],
    keep: i64,
) -> Result<usize> {
    if posts.is_empty() {
        return Ok(0);
    }
    retry_locked(|| record_trigger_posts_tx(pool, posts, keep)).await
}

/// One attempt at `record_trigger_posts`, as a database-level result so the
/// caller can tell a held write lock from any other failure.
async fn record_trigger_posts_tx(
    pool: &SqlitePool,
    posts: &[TriggerPost],
    keep: i64,
) -> std::result::Result<usize, sqlx::Error> {
    let now = timeutil::fmt_state_ts(timeutil::now());
    // Which posts are new is counted before the write transaction: a
    // transaction that reads and then writes fails at once when another
    // connection writes in between (its read snapshot can no longer be
    // upgraded to a write), and the count is only for the log line anyway.
    let mut fresh = 0usize;
    let mut counted = std::collections::HashSet::new();
    for post in posts {
        let existed: Option<(String,)> =
            sqlx::query_as("SELECT first_seen FROM trigger_posts WHERE uri = ?")
                .bind(&post.uri)
                .fetch_optional(pool)
                .await?;
        if existed.is_none() && counted.insert(post.uri.as_str()) {
            fresh += 1;
        }
    }
    let mut tx = pool.begin().await?;
    for post in posts {
        sqlx::query(
            "INSERT INTO trigger_posts (uri, url, acct, author_url, replies_count, created_at, in_reply_to_id, in_reply_to_account_id, is_public, is_boost, mentions, source, first_seen, last_seen)
                VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                ON CONFLICT(uri) DO UPDATE SET
                    url = excluded.url,
                    acct = excluded.acct,
                    author_url = excluded.author_url,
                    replies_count = excluded.replies_count,
                    created_at = excluded.created_at,
                    in_reply_to_id = excluded.in_reply_to_id,
                    in_reply_to_account_id = excluded.in_reply_to_account_id,
                    is_public = excluded.is_public,
                    is_boost = excluded.is_boost,
                    mentions = excluded.mentions,
                    last_seen = excluded.last_seen",
        )
        .bind(&post.uri)
        .bind(&post.url)
        .bind(&post.acct)
        .bind(&post.author_url)
        .bind(post.replies_count)
        .bind(&post.created_at)
        .bind(&post.in_reply_to_id)
        .bind(&post.in_reply_to_account_id)
        .bind(post.is_public)
        .bind(post.is_boost)
        .bind(&post.mentions)
        .bind(&post.source)
        .bind(&now)
        .bind(&now)
        .execute(&mut *tx)
        .await?;
    }
    // keep only the newest rows, so the table cannot grow forever
    sqlx::query(
        "DELETE FROM trigger_posts WHERE uri NOT IN
            (SELECT uri FROM trigger_posts ORDER BY last_seen DESC, uri ASC LIMIT ?)",
    )
    .bind(keep)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(fresh)
}

/// One trigger post, as the table records it, or None when the database has
/// never scanned it.
pub async fn trigger_post(pool: &SqlitePool, uri: &str) -> Result<Option<TriggerPost>> {
    let row = sqlx::query(
        "SELECT uri, url, acct, author_url, replies_count, created_at, in_reply_to_id, in_reply_to_account_id, is_public, is_boost, mentions, source FROM trigger_posts WHERE uri = ?",
    )
    .bind(uri)
    .fetch_optional(pool)
    .await
    ?;
    Ok(row.map(|row| TriggerPost {
        uri: row.get("uri"),
        url: row.get("url"),
        acct: row.get("acct"),
        author_url: row.get("author_url"),
        replies_count: row.get("replies_count"),
        created_at: row.get("created_at"),
        in_reply_to_id: row.get("in_reply_to_id"),
        in_reply_to_account_id: row.get("in_reply_to_account_id"),
        is_public: row.get("is_public"),
        is_boost: row.get("is_boost"),
        mentions: row.get("mentions"),
        source: row.get("source"),
    }))
}

/// Read one seen_hosts row, whose fields are columns.
fn host_row(row: &sqlx::sqlite::SqliteRow) -> HostRow {
    HostRow {
        host: row.get("host"),
        webserver: row.get("webserver"),
        software: row.get("software"),
        version: row.get("version"),
        mastodon_api: row.get("mastodon_api"),
        misskey_api: row.get("misskey_api"),
        lemmy_api: row.get("lemmy_api"),
        peertube_api: row.get("peertube_api"),
        last_checked: row.get("last_checked"),
        error: row.get("error"),
        raw_nodeinfo: row.get("raw_nodeinfo"),
    }
}

/// Write one seen_hosts row, whose fields are columns.
async fn insert_host_row<'e, E>(executor: E, row: &HostRow) -> std::result::Result<(), sqlx::Error>
where
    E: sqlx::Executor<'e, Database = sqlx::Sqlite>,
{
    sqlx::query(
        "INSERT INTO seen_hosts (host, webserver, software, version, mastodon_api, misskey_api, lemmy_api, peertube_api, last_checked, error, raw_nodeinfo)
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&row.host)
    .bind(&row.webserver)
    .bind(&row.software)
    .bind(&row.version)
    .bind(row.mastodon_api)
    .bind(row.misskey_api)
    .bind(row.lemmy_api)
    .bind(row.peertube_api)
    .bind(&row.last_checked)
    .bind(&row.error)
    .bind(&row.raw_nodeinfo)
    .execute(executor)
    .await?;
    Ok(())
}

/// The whole remembered state, read back from the database. Expiry and the
/// derived collections are applied by the caller (`StateStore::load`).
pub async fn load_state(pool: &SqlitePool) -> Result<State> {
    let mut state = State::default();
    *state.seen_urls_mut() = load_timestamped_set(pool, "seen_urls", "uri").await?;
    *state.known_followings_mut() = load_timestamped_set(pool, "known_followings", "acct").await?;
    *state.recently_checked_users_mut() =
        load_timestamped_set(pool, "recently_checked_users", "acct").await?;

    let rows = sqlx::query(
        "SELECT host, webserver, software, version, mastodon_api, misskey_api, lemmy_api, peertube_api, last_checked, error, raw_nodeinfo FROM seen_hosts ORDER BY rowid",
    )
    .fetch_all(pool)
    .await
    ?;
    for row in rows {
        let host_row = host_row(&row);
        state
            .seen_hosts_mut()
            .add(&host_row.host, host_row.to_state_value());
    }

    let rows = sqlx::query("SELECT uri, last_seen, created_at FROM context_cache ORDER BY rowid")
        .fetch_all(pool)
        .await?;
    for row in rows {
        let uri: String = row.get("uri");
        let last_seen = timeutil::parse(&row.get::<String, _>("last_seen"));
        let created_at = timeutil::parse(&row.get::<String, _>("created_at"));
        if let (Some(last_seen), Some(created_at)) = (last_seen, created_at) {
            state
                .recently_checked_context_mut()
                .record(&uri, last_seen, created_at);
        }
    }

    let rows =
        sqlx::query("SELECT url, remote_url, server, post_id FROM replied_toots ORDER BY rowid")
            .fetch_all(pool)
            .await?;
    for row in rows {
        let url: String = row.get("url");
        let replied = match (
            row.get::<Option<String>, _>("remote_url"),
            row.get::<Option<String>, _>("server"),
            row.get::<Option<String>, _>("post_id"),
        ) {
            (Some(remote_url), Some(server), Some(post_id)) => Some(RepliedToot {
                url: remote_url,
                post_ref: PostRef::new(server, post_id),
            }),
            // all-NULL columns remember a lookup that found nothing
            _ => None,
        };
        state.replied_toot_server_ids_mut().insert(url, replied);
    }

    Ok(state)
}

/// Write the whole remembered state back, replacing what was there. The
/// append-only collections keep only their newest `max_entries` items.
pub async fn save_state(pool: &SqlitePool, state: &State, max_entries: usize) -> Result<()> {
    retry_locked(|| save_state_tx(pool, state, max_entries)).await
}

/// One attempt at `save_state`, as a database-level result so the caller
/// can tell a held write lock from any other failure. The transaction is
/// long when the remembered state is large, so it is the write most likely
/// to find the lock held — and to hold it for everyone else.
async fn save_state_tx(
    pool: &SqlitePool,
    state: &State,
    max_entries: usize,
) -> std::result::Result<(), sqlx::Error> {
    let mut tx = pool.begin().await?;

    save_timestamped_set(&mut tx, "seen_urls", "uri", state.seen_urls(), max_entries).await?;
    save_timestamped_set(
        &mut tx,
        "known_followings",
        "acct",
        state.known_followings(),
        max_entries,
    )
    .await?;
    save_timestamped_set(
        &mut tx,
        "recently_checked_users",
        "acct",
        state.recently_checked_users(),
        max_entries,
    )
    .await?;

    sqlx::query("DELETE FROM seen_hosts")
        .execute(&mut *tx)
        .await?;
    for (host, info) in state.seen_hosts().iter() {
        insert_host_row(&mut *tx, &HostRow::from_state_value(host, info)).await?;
    }

    sqlx::query("DELETE FROM context_cache")
        .execute(&mut *tx)
        .await?;
    for (uri, entry) in state.recently_checked_context().entries() {
        sqlx::query("INSERT INTO context_cache (uri, last_seen, created_at) VALUES (?, ?, ?)")
            .bind(uri)
            .bind(timeutil::fmt_state_ts(entry.last_seen))
            .bind(timeutil::fmt_state_ts(entry.created_at))
            .execute(&mut *tx)
            .await?;
    }

    sqlx::query("DELETE FROM replied_toots")
        .execute(&mut *tx)
        .await?;
    let replied: Vec<_> = state.replied_toot_server_ids().iter().collect();
    let start = replied.len().saturating_sub(max_entries);
    for (url, toot) in &replied[start..] {
        let (remote_url, server, post_id) = match toot {
            Some(RepliedToot {
                url: remote_url,
                post_ref,
            }) => (
                Some(remote_url.clone()),
                Some(post_ref.server.clone()),
                Some(post_ref.post_id.clone()),
            ),
            None => (None, None, None),
        };
        sqlx::query(
            "INSERT INTO replied_toots (url, remote_url, server, post_id) VALUES (?, ?, ?, ?)",
        )
        .bind(*url)
        .bind(remote_url)
        .bind(server)
        .bind(post_id)
        .execute(&mut *tx)
        .await?;
    }

    tx.commit().await?;
    Ok(())
}

/// One timestamped-set table, in insertion order. Timestamps are the
/// Python `str(datetime)` shape, the way the state files wrote them.
async fn load_timestamped_set(
    pool: &SqlitePool,
    table: &str,
    key_column: &str,
) -> Result<TimestampedSet> {
    let sql = format!("SELECT {key_column}, first_seen FROM {table} ORDER BY rowid");
    let rows = sqlx::query(AssertSqlSafe(sql)).fetch_all(pool).await?;
    let mut set = TimestampedSet::new();
    for row in rows {
        let item: String = row.get(key_column);
        let first_seen: String = row.get("first_seen");
        set.add(&item, timeutil::parse(&first_seen));
    }
    Ok(set)
}

/// Replace one timestamped-set table with the set's newest `max_entries`
/// items, in the set's order.
async fn save_timestamped_set(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    table: &str,
    key_column: &str,
    set: &TimestampedSet,
    max_entries: usize,
) -> std::result::Result<(), sqlx::Error> {
    let delete = format!("DELETE FROM {table}");
    sqlx::query(AssertSqlSafe(delete))
        .execute(&mut **tx)
        .await?;
    let entries: Vec<_> = set.entries().collect();
    let start = entries.len().saturating_sub(max_entries);
    let insert = format!("INSERT INTO {table} ({key_column}, first_seen) VALUES (?, ?)");
    for (item, when) in &entries[start..] {
        sqlx::query(AssertSqlSafe(insert.clone()))
            .bind(*item)
            .bind(timeutil::fmt_state_ts(*when))
            .execute(&mut **tx)
            .await?;
    }
    Ok(())
}

/// A cached robots.txt, by the URL it was fetched from.
pub async fn robots_cached(pool: &SqlitePool, url: &str) -> Result<Option<String>> {
    let row: Option<(String,)> = sqlx::query_as("SELECT text FROM robots_cache WHERE url = ?")
        .bind(url)
        .fetch_optional(pool)
        .await?;
    Ok(row.map(|(text,)| text))
}

/// Remember a fetched robots.txt, replacing any earlier copy.
pub async fn robots_store(pool: &SqlitePool, url: &str, text: &str) -> Result<()> {
    let fetched_at = timeutil::fmt_state_ts(timeutil::now());
    sqlx::query(
        "INSERT INTO robots_cache (url, text, fetched_at) VALUES (?, ?, ?)
            ON CONFLICT(url) DO UPDATE SET text = excluded.text, fetched_at = excluded.fetched_at",
    )
    .bind(url)
    .bind(text)
    .bind(&fetched_at)
    .execute(pool)
    .await?;
    Ok(())
}

/// Delete cached robots.txt entries older than `max_age`; how many went.
pub async fn robots_discard_stale(pool: &SqlitePool, max_age: time::Duration) -> Result<u64> {
    let cutoff = timeutil::fmt_state_ts(timeutil::now() - max_age);
    let result = sqlx::query("DELETE FROM robots_cache WHERE fetched_at < ?")
        .bind(&cutoff)
        .execute(pool)
        .await?;
    Ok(result.rows_affected())
}

/// A pool that connects on first use to a throwaway in-memory database, for
/// tests that build clients but never query them.
#[cfg(test)]
pub fn memory_pool() -> SqlitePool {
    SqlitePoolOptions::new()
        .connect_lazy("sqlite::memory:")
        .unwrap()
}

#[cfg(test)]
mod tests;
