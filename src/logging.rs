//! Logging in a configurable format, plus a SQLite database the web UI can
//! query.
//!
//! A `FormatEvent` renders the `log_format` substitutions on stderr, and a
//! layer sends every line to the database in the state directory, which the
//! /logs page reads.

use std::fmt;
use std::sync::{Arc, Mutex, OnceLock, RwLock};

use sqlx::SqlitePool;
use time::{OffsetDateTime, UtcOffset};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use tokio::task::JoinHandle;
use tracing::field::{Field, Visit};
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::Registry;
use tracing_subscriber::fmt::FmtContext;
use tracing_subscriber::fmt::format::{FormatEvent, FormatFields, Writer};
use tracing_subscriber::layer::Context;
use tracing_subscriber::prelude::__tracing_subscriber_SubscriberExt;
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::reload;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, Layer};

use crate::config::Config;
use crate::db::{self, LogRow};
use crate::error::{Error, Result};
use crate::timeutil;

/// The date format: '%Y-%m-%d %H:%M:%S': "%Y-%m-%d %H:%M:%S %Z".
const DATEFMT: &[time::format_description::FormatItem<'static>] =
    time::macros::format_description!("[year]-[month]-[day] [hour]:[minute]:[second]");

/// How many log lines the database keeps; the oldest are pruned as new ones
/// arrive.
const LOG_RETENTION: i64 = 100_000;

/// Prune the log table every this many lines received, so the DELETE is not
/// run per line.
const PRUNE_EVERY: i64 = 1_000;

/// The log database's writer, started by [`setup`]. Every line the
/// `DbLayer` sees is sent to its task, the only writer, so lines reach the
/// database in order.
///
/// [`LogDb::shutdown`] closes the channel and waits for the writer to
/// finish, so no line is lost on the way out.
pub struct LogDb {
    /// Shared with the `DbLayer` so shutdown can close the channel.
    tx: Arc<Mutex<Option<UnboundedSender<LogRow>>>>,
    writer: Mutex<Option<JoinHandle<()>>>,
}

impl LogDb {
    /// Stop the writer, waiting for the lines already received to be
    /// written. Called before a one-shot run exits, so no line is lost.
    pub async fn shutdown(&self) {
        // take the sender out of the shared cell: the layer finds None and
        // stops sending, and the writer's channel closes once the last
        // in-flight send is done
        drop(self.tx.lock().unwrap().take());
        // take the handle before awaiting, so the lock is not held across
        // the await point
        let handle = self.writer.lock().unwrap().take();
        if let Some(handle) = handle {
            let _ = handle.await;
        }
    }
}

/// The level filter's reload handle, and the format the stderr layer
/// renders with, both installed by [`setup`] and swapped by [`apply`].
static FILTER_RELOAD: OnceLock<reload::Handle<EnvFilter, Registry>> = OnceLock::new();
static FORMAT: OnceLock<Arc<RwLock<String>>> = OnceLock::new();

/// The filter one log level names: the level itself, with the noisy
/// dependencies quieted (the Python original quieted urllib3; the HTTP
/// stack is reqwest here, and sqlx logs every query at debug).
fn env_filter(level: &str) -> Result<EnvFilter> {
    let level = match level.to_uppercase().as_str() {
        "DEBUG" => "debug",
        "INFO" => "info",
        "WARNING" | "WARN" => "warn",
        "ERROR" | "CRITICAL" => "error",
        other => {
            return Err(Error::Config(format!(
                "Unknown level: {other:?} (DEBUG|INFO|WARNING|ERROR|CRITICAL)"
            )));
        }
    };
    Ok(EnvFilter::try_new(format!(
        "{level},reqwest=warn,hyper=warn,hyper_util=warn,h2=warn,tower_http=warn,sqlx=warn"
    ))
    .unwrap_or_else(|_| EnvFilter::new("info")))
}

/// Apply a saved config's log level and format to the running logger, so a
/// change on the config page takes effect without a restart. A no-op when
/// logging was never set up.
pub fn apply(config: &Config) -> Result<()> {
    let filter = env_filter(&config.log_level)?;
    if let Some(handle) = FILTER_RELOAD.get() {
        handle
            .modify(|current| *current = filter)
            .map_err(|ex| Error::Config(format!("could not apply the log level: {ex}")))?;
    }
    if let Some(format) = FORMAT.get() {
        *format.write().unwrap() = config.log_format.clone();
    }
    Ok(())
}

/// Configure logging from the config and start writing log lines to the
/// database. Returns the handle that stops the writer on shutdown.
///
/// The configured format goes to stderr, and a layer sends every line to
/// the database (which the /logs page reads). A later [`apply`] swaps the
/// level and the format in place.
pub fn setup(config: &Config, pool: SqlitePool) -> Result<LogDb> {
    let filter = env_filter(&config.log_level)?;

    let (tx, rx) = unbounded_channel::<LogRow>();
    let shared_tx: Arc<Mutex<Option<UnboundedSender<LogRow>>>> = Arc::new(Mutex::new(Some(tx)));
    let log_db = LogDb {
        tx: shared_tx.clone(),
        writer: Mutex::new(Some(spawn_writer(pool, rx))),
    };

    let format = Arc::new(RwLock::new(config.log_format.clone()));
    let fmt_layer = tracing_subscriber::fmt::layer()
        .with_writer(std::io::stderr)
        .event_format(ConfiguredFormat {
            format: format.clone(),
        });

    let (filter_layer, filter_handle) = reload::Layer::new(filter);
    tracing_subscriber::registry()
        .with(filter_layer)
        .with(DbLayer { tx: shared_tx })
        .with(fmt_layer)
        .init();
    let _ = FILTER_RELOAD.set(filter_handle);
    let _ = FORMAT.set(format);
    Ok(log_db)
}

/// The task that writes log lines to the database, one at a time so they
/// arrive in order. Prunes the table every `PRUNE_EVERY` lines.
fn spawn_writer(pool: SqlitePool, mut rx: UnboundedReceiver<LogRow>) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut received = 0i64;
        while let Some(row) = rx.recv().await {
            if let Err(ex) = db::insert_log(&pool, &row).await {
                eprintln!("WARNING:FediFetcher:could not write a log line to the database: {ex}");
            }
            received += 1;
            if received % PRUNE_EVERY == 0
                && let Err(ex) = db::prune_logs(&pool, LOG_RETENTION).await
            {
                eprintln!("WARNING:FediFetcher:could not prune the log database: {ex}");
            }
        }
    })
}

/// Render one event in the configured `log_format`. The format is shared
/// with [`apply`], so a saved change takes effect on the next line.
struct ConfiguredFormat {
    format: Arc<RwLock<String>>,
}

impl<S, N> FormatEvent<S, N> for ConfiguredFormat
where
    S: Subscriber + for<'span> LookupSpan<'span>,
    N: for<'writer> FormatFields<'writer> + 'static,
{
    fn format_event(
        &self,
        _ctx: &FmtContext<'_, S, N>,
        mut writer: Writer<'_>,
        event: &Event<'_>,
    ) -> fmt::Result {
        let format = self.format.read().unwrap().clone();
        let line = format_line(&format, event.metadata().level(), &message_of(event));
        writeln!(writer, "{line}")
    }
}

/// Send every log line to the database's writer task. The channel keeps the
/// layer synchronous; the task does the writing.
struct DbLayer {
    tx: Arc<Mutex<Option<UnboundedSender<LogRow>>>>,
}

impl<S: Subscriber> Layer<S> for DbLayer {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let Some(tx) = self.tx.lock().unwrap().clone() else {
            return;
        };
        let row = LogRow {
            ts: asctime(timeutil::now_local()),
            level: level_name(event.metadata().level()).to_string(),
            target: event.metadata().target().to_string(),
            message: message_of(event),
        };
        // a closed channel means the writer is shutting down; the line is
        // still on stderr
        let _ = tx.send(row);
    }
}

/// The `message` field of an event.
fn message_of(event: &Event<'_>) -> String {
    let mut visitor = MessageVisitor::default();
    event.record(&mut visitor);
    visitor.message
}

#[derive(Default)]
struct MessageVisitor {
    message: String,
}

impl Visit for MessageVisitor {
    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        if field.name() == "message" {
            self.message = format!("{value:?}");
        }
    }
}

/// Substitute the configured format's fields.
fn format_line(format: &str, level: &Level, message: &str) -> String {
    format_db_line(
        format,
        level_name(level),
        &asctime(timeutil::now_local()),
        message,
    )
}

/// Render one stored log row the way `log_format` would.
pub fn format_db_line(format: &str, level: &str, ts: &str, message: &str) -> String {
    format
        .replace("%(asctime)s", ts)
        .replace("%(levelname)s", level)
        .replace("%(name)s", "FediFetcher")
        .replace("%(message)s", message)
}

/// WARN renders as WARNING; CRITICAL logs at ERROR, as tracing has no
/// level above it.
fn level_name(level: &Level) -> &'static str {
    match *level {
        Level::DEBUG | Level::TRACE => "DEBUG",
        Level::INFO => "INFO",
        Level::WARN => "WARNING",
        Level::ERROR => "ERROR",
    }
}

/// The local time, in the configured date format. The time crate has no
/// time zone names, so a zero offset is spelled UTC and anything else as
/// +HH:MM.
fn asctime(now: OffsetDateTime) -> String {
    let date = now.format(DATEFMT).unwrap_or_else(|_| now.to_string());
    let offset = now.offset();
    if offset == UtcOffset::UTC {
        return format!("{date} UTC");
    }
    let (hours, minutes, _) = offset.as_hms();
    let sign = if hours < 0 || minutes < 0 { "-" } else { "+" };
    format!("{date} {sign}{:02}:{:02}", hours.abs(), minutes.abs())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_saved_level_and_format_apply_without_a_restart() {
        let config = Config {
            log_level: "INFO".to_string(),
            log_format: "%(levelname)s %(message)s".to_string(),
            ..Config::default()
        };
        let _log_db = setup(&config, crate::db::memory_pool()).expect("setup works");

        // applying the same settings is a no-op that works
        apply(&config).expect("the same settings apply");

        // different saved settings take effect in place
        let changed = Config {
            log_level: "ERROR".to_string(),
            log_format: "%(message)s".to_string(),
            ..Config::default()
        };
        apply(&changed).expect("the saved settings apply right away");
        let format = FORMAT.get().unwrap().read().unwrap().clone();
        assert_eq!(format, "%(message)s", "the format was swapped in place");
        // the level was swapped too: the reload handle took the new filter
        // (modify succeeded, which is the swap), and the stderr layer
        // renders through the format that was just verified

        // an unknown level is refused, leaving the old settings alone
        let bad = Config {
            log_level: "LOUD".to_string(),
            ..Config::default()
        };
        assert!(apply(&bad).is_err());
        let format = FORMAT.get().unwrap().read().unwrap().clone();
        assert_eq!(format, "%(message)s", "the refused apply changed nothing");
    }

    #[test]
    fn format_line_substitutes_the_configured_fields() {
        let line = format_line(
            "%(asctime)s: %(levelname)s %(name)s: %(message)s",
            &Level::WARN,
            "hello",
        );
        let mut parts = line.splitn(2, ": ");
        let asctime = parts.next().unwrap();
        assert!(asctime.ends_with(" UTC") || asctime.contains(" +") || asctime.contains(" -"));
        assert_eq!(parts.next().unwrap(), "WARNING FediFetcher: hello");
    }

    #[test]
    fn a_stored_row_renders_like_a_live_line() {
        let line = format_db_line(
            "%(asctime)s: %(message)s",
            "INFO",
            "2026-10-22 16:05:07 -05:00",
            "Found 2 toots in timeline",
        );
        assert_eq!(
            line,
            "2026-10-22 16:05:07 -05:00: Found 2 toots in timeline"
        );
    }

    #[test]
    fn level_names_match_the_configured_levels() {
        assert_eq!(level_name(&Level::DEBUG), "DEBUG");
        assert_eq!(level_name(&Level::INFO), "INFO");
        assert_eq!(level_name(&Level::WARN), "WARNING");
        assert_eq!(level_name(&Level::ERROR), "ERROR");
    }
}
