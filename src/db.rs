//! The SQLite database in the state directory: the log lines, and the web
//! UI's run history. The text files in the state directory stay the state of
//! record for what FediFetcher has seen; this is the append-only record of
//! what it did, in one file (`fedifetcher.db`) that the web UI can query.

use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePool, SqlitePoolOptions};
use sqlx::Row;

use crate::config::Config;
use crate::error::{Error, Result};

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

/// Open the database, creating it and its tables if they are not there yet.
pub async fn open(config: &Config) -> Result<SqlitePool> {
    std::fs::create_dir_all(&config.state_dir)
        .map_err(|ex| Error(format!("Could not create {}: {ex}", config.state_dir.display())))?;
    let path = config.state_dir.join("fedifetcher.db");
    let pool = SqlitePoolOptions::new()
        .max_connections(5)
        .connect_with(
            SqliteConnectOptions::new()
                .filename(&path)
                .create_if_missing(true)
                // WAL lets the web UI read while a run is writing
                .journal_mode(SqliteJournalMode::Wal)
                .busy_timeout(std::time::Duration::from_secs(5)),
        )
        .await
        .map_err(|ex| Error(format!("Could not open {}: {ex}", path.display())))?;
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS logs (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            ts TEXT NOT NULL,
            level TEXT NOT NULL,
            target TEXT NOT NULL,
            message TEXT NOT NULL
        )",
    )
    .execute(&pool)
    .await
    .map_err(db_error)?;
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS runs (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            started TEXT NOT NULL,
            finished TEXT NOT NULL,
            outcome TEXT NOT NULL,
            message TEXT NOT NULL
        )",
    )
    .execute(&pool)
    .await
    .map_err(db_error)?;
    Ok(pool)
}

/// Add one log line.
pub async fn insert_log(pool: &SqlitePool, row: &LogRow) -> Result<()> {
    sqlx::query("INSERT INTO logs (ts, level, target, message) VALUES (?, ?, ?, ?)")
        .bind(&row.ts)
        .bind(&row.level)
        .bind(&row.target)
        .bind(&row.message)
        .execute(pool)
        .await
        .map_err(db_error)?;
    Ok(())
}

/// Which column of the log table the /logs page sorts by.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LogSort {
    Time,
    Level,
    Message,
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
        .await
        .map_err(db_error)?;
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
    sqlx::query("DELETE FROM logs")
        .execute(pool)
        .await
        .map_err(db_error)?;
    Ok(())
}

/// Keep only the newest `keep` log lines.
pub async fn prune_logs(pool: &SqlitePool, keep: i64) -> Result<()> {
    sqlx::query("DELETE FROM logs WHERE id <= (SELECT COALESCE(MAX(id), 0) - ? FROM logs)")
        .bind(keep)
        .execute(pool)
        .await
        .map_err(db_error)?;
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
        .await
        .map_err(db_error)?;
    Ok(())
}

/// The most recent runs, newest first.
pub async fn recent_runs(pool: &SqlitePool, max: usize) -> Result<Vec<RunRow>> {
    let rows = sqlx::query(
        "SELECT started, finished, outcome, message FROM runs ORDER BY id DESC LIMIT ?",
    )
    .bind(max as i64)
    .fetch_all(pool)
    .await
    .map_err(db_error)?;
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

fn db_error(ex: sqlx::Error) -> Error {
    Error(format!("Database error: {ex}"))
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
            insert_log(&pool, &log_row(&format!("line {n}"))).await.unwrap();
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
            insert_log(&pool, &log_row(&format!("line {n}"))).await.unwrap();
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
            insert_log(&pool, &log_row(&format!("line {n}"))).await.unwrap();
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
}
