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

/// The most recent log lines, sorted as the /logs page asks. The newest
/// `max` lines are fetched, then that page is sorted by `sort`; lines equal
/// on the sorted column keep their newest-first order.
pub async fn recent_logs(
    pool: &SqlitePool,
    max: usize,
    sort: LogSort,
    order: LogOrder,
) -> Result<Vec<LogRow>> {
    let rows = sqlx::query("SELECT ts, level, target, message FROM logs ORDER BY id DESC LIMIT ?")
        .bind(max as i64)
        .fetch_all(pool)
        .await?;
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
mod tests {
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

        let lines = recent_logs(&pool, 10, LogSort::Time, LogOrder::Desc)
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
        let lines = recent_logs(&pool, 10, LogSort::Time, LogOrder::Asc)
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
        let lines = recent_logs(&pool, 2, LogSort::Time, LogOrder::Desc)
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
        let ascending = recent_logs(&pool, 10, LogSort::Level, LogOrder::Asc)
            .await
            .unwrap();
        let messages: Vec<&str> = ascending.iter().map(|row| row.message.as_str()).collect();
        assert_eq!(messages, ["b", "c", "a"], "DEBUG, ERROR, INFO");
        let descending = recent_logs(&pool, 10, LogSort::Level, LogOrder::Desc)
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
        let lines = recent_logs(&pool, 10, LogSort::Message, LogOrder::Asc)
            .await
            .unwrap();
        let messages: Vec<&str> = lines.iter().map(|row| row.message.as_str()).collect();
        assert_eq!(messages, ["apple", "mango", "zebra"]);
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
        assert!(recent_logs(&pool, 10, LogSort::Time, LogOrder::Desc)
            .await
            .unwrap()
            .is_empty());
        // clearing an empty log is fine, and new lines still arrive
        clear_logs(&pool).await.unwrap();
        insert_log(&pool, &log_row("after clearing")).await.unwrap();
        let lines = recent_logs(&pool, 10, LogSort::Time, LogOrder::Desc)
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
        let lines = recent_logs(&pool, 10, LogSort::Time, LogOrder::Desc)
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
        assert!(recent_logs(&pool, 10, LogSort::Time, LogOrder::Desc)
            .await
            .unwrap()
            .is_empty());
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

        assert!(!is_private_post(&pool, "https://remote.example/@a/1")
            .await
            .unwrap());
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
        assert!(is_private_post(&pool, "https://remote.example/@a/1")
            .await
            .unwrap());
        assert!(!is_private_post(&pool, "https://remote.example/@a/2")
            .await
            .unwrap());
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

        assert!(trigger_post(&pool, "https://remote.example/@a/1")
            .await
            .unwrap()
            .is_none());
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

        let insert =
            "INSERT INTO logs (ts, level, target, message) VALUES ('t', 'INFO', 'test', 'm')";

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
        let info = crate::servers::ServerInfo::from_state(
            loaded.seen_hosts().get("remote.example").unwrap(),
        )
        .unwrap();
        assert_eq!(info.software, "mastodon");
        assert_eq!(info.version, "4.3.0");
        assert!(info.supports(crate::servers::ApiFlavor::Mastodon));
        assert!(
            crate::servers::ServerInfo::from_state(
                loaded.seen_hosts().get("down.example").unwrap()
            )
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
        let info = crate::servers::ServerInfo::from_state(
            state.seen_hosts().get("remote.example").unwrap(),
        )
        .unwrap();
        assert_eq!(info.software, "mastodon");
        assert!(info.supports(crate::servers::ApiFlavor::Mastodon));
        assert!(crate::servers::ServerInfo::from_state(
            state.seen_hosts().get("down.example").unwrap()
        )
        .is_none());
    }

    #[tokio::test]
    async fn the_database_survives_reopening() {
        let (config, _dir) = test_config();
        let pool = open(&config).await.unwrap();
        insert_log(&pool, &log_row("remember me")).await.unwrap();
        drop(pool);

        let pool = open(&config).await.unwrap();
        let lines = recent_logs(&pool, 10, LogSort::Time, LogOrder::Desc)
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
        sqlx::query(
            "INSERT INTO scratch (name, note) VALUES ('x', NULL), ('y', 'text'), ('z', 42)",
        )
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
        assert!(record_known_context_posts(&pool, &[])
            .await
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn known_context_posts_keep_first_seen_and_refresh_last_seen() {
        let (config, _dir) = test_config();
        let pool = open(&config).await.unwrap();

        let uri = "https://remote.example/@a/1";
        let long_ago = timeutil::fmt_state_ts(timeutil::now() - time::Duration::days(30));
        sqlx::query(
            "INSERT INTO known_context_posts (uri, first_seen, last_seen) VALUES (?, ?, ?)",
        )
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

        assert!(table_rows(
            &pool,
            "logs; DROP TABLE logs",
            &columns,
            "id",
            LogOrder::Asc,
            10
        )
        .await
        .is_err());
        assert!(table_rows(
            &pool,
            "logs",
            &columns,
            "id; DROP TABLE logs",
            LogOrder::Asc,
            10
        )
        .await
        .is_err());
        assert!(
            table_rows(&pool, "logs", &columns, "not_a_column", LogOrder::Asc, 10)
                .await
                .is_err(),
            "a sort column must be a real column"
        );
        assert!(table_columns(&pool, "logs\"; DROP TABLE logs")
            .await
            .is_err());
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
}
