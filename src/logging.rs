//! Logging that looks like the Python original's, plus a ring buffer the web
//! UI can read.
//!
//! Python configures the root logger with `log_format` and the date format
//! "%Y-%m-%d %H:%M:%S %Z"; here a `FormatEvent` renders the same
//! substitutions, and a second layer keeps the last lines for /logs.

use std::collections::VecDeque;
use std::fmt;
use std::sync::{Arc, Mutex, OnceLock};

use time::{OffsetDateTime, UtcOffset};
use tracing::field::{Field, Visit};
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::fmt::format::{FormatEvent, FormatFields, Writer};
use tracing_subscriber::fmt::FmtContext;
use tracing_subscriber::layer::Context;
use tracing_subscriber::prelude::__tracing_subscriber_SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::{EnvFilter, Layer};

use crate::config::Config;
use crate::error::ConfigError;
use crate::timeutil;

/// The most log lines kept for the web UI's /logs page.
pub const LOG_RING_CAPACITY: usize = 1000;

/// Python's datefmt: "%Y-%m-%d %H:%M:%S %Z".
const DATEFMT: &[time::format_description::FormatItem<'static>] = time::macros::format_description!(
    "[year]-[month]-[day] [hour]:[minute]:[second]"
);

static LOG_LINES: OnceLock<Arc<Mutex<VecDeque<String>>>> = OnceLock::new();

fn log_lines() -> Arc<Mutex<VecDeque<String>>> {
    LOG_LINES
        .get_or_init(|| Arc::new(Mutex::new(VecDeque::new())))
        .clone()
}

/// The most recent log lines, oldest first.
pub fn recent_log_lines(max: usize) -> Vec<String> {
    let ring = log_lines();
    let ring = ring.lock().unwrap();
    ring.iter().rev().take(max).rev().cloned().collect()
}

/// Configure logging from the config: the Python-style format on stderr, and
/// the ring buffer for the web UI.
pub fn setup_logging(config: &Config) -> Result<(), ConfigError> {
    let level = match config.log_level.to_uppercase().as_str() {
        "DEBUG" => "debug",
        "INFO" => "info",
        "WARNING" | "WARN" => "warn",
        "ERROR" | "CRITICAL" => "error",
        other => {
            return Err(ConfigError(format!(
                "Unknown level: {other:?} (DEBUG|INFO|WARNING|ERROR|CRITICAL)"
            )))
        }
    };

    // the Python original quiets urllib3; the HTTP stack is reqwest here
    let filter = EnvFilter::try_new(format!(
        "{level},reqwest=warn,hyper=warn,hyper_util=warn,h2=warn,tower_http=warn"
    ))
    .unwrap_or_else(|_| EnvFilter::new("info"));

    let ring_layer = RingLayer {
        ring: log_lines(),
        format: config.log_format.clone(),
    };
    let fmt_layer = tracing_subscriber::fmt::layer()
        .with_writer(std::io::stderr)
        .event_format(PythonFormat {
            format: config.log_format.clone(),
        });

    tracing_subscriber::registry()
        .with(filter)
        .with(ring_layer)
        .with(fmt_layer)
        .init();
    Ok(())
}

/// Render one event the way Python's `log_format` would.
struct PythonFormat {
    format: String,
}

impl<S, N> FormatEvent<S, N> for PythonFormat
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
        let line = format_line(&self.format, event.metadata().level(), &message_of(event));
        writeln!(writer, "{line}")
    }
}

/// Keep the last lines for the web UI.
struct RingLayer {
    ring: Arc<Mutex<VecDeque<String>>>,
    format: String,
}

impl<S: Subscriber> Layer<S> for RingLayer {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let line = format_line(&self.format, event.metadata().level(), &message_of(event));
        let mut ring = self.ring.lock().unwrap();
        if ring.len() >= LOG_RING_CAPACITY {
            ring.pop_front();
        }
        ring.push_back(line);
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

/// Substitute the Python format's fields into `format`.
fn format_line(format: &str, level: &Level, message: &str) -> String {
    format
        .replace("%(asctime)s", &asctime(timeutil::now_local()))
        .replace("%(levelname)s", level_name(level))
        .replace("%(name)s", "FediFetcher")
        .replace("%(message)s", message)
}

/// Python renders WARN as WARNING; CRITICAL logs at ERROR, as tracing has no
/// level above it.
fn level_name(level: &Level) -> &'static str {
    match *level {
        Level::DEBUG | Level::TRACE => "DEBUG",
        Level::INFO => "INFO",
        Level::WARN => "WARNING",
        Level::ERROR => "ERROR",
    }
}

/// The local time, the way Python's datefmt spells it. The time crate has no
/// time zone names, so a zero offset is spelled UTC and anything else as
/// +HH:MM.
fn asctime(now: OffsetDateTime) -> String {
    let date = now
        .format(DATEFMT)
        .unwrap_or_else(|_| now.to_string());
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

    #[test]
    fn format_line_substitutes_the_python_fields() {
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
    fn level_names_match_pythons() {
        assert_eq!(level_name(&Level::DEBUG), "DEBUG");
        assert_eq!(level_name(&Level::INFO), "INFO");
        assert_eq!(level_name(&Level::WARN), "WARNING");
        assert_eq!(level_name(&Level::ERROR), "ERROR");
    }
}
