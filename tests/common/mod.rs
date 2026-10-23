//! Shared helpers for the integration tests.

// Each integration test binary compiles this module, but only some use every
// helper (LogCapture is only used by the http and backfill suites).
#![allow(dead_code)]

use std::path::{Path, PathBuf};

use fedifetcher::api::mastodon::HomeServer;
use fedifetcher::config::Config;
use fedifetcher::http::HttpClient;
use fedifetcher::run::{CancelToken, Context};
use fedifetcher::store::State;

/// A fresh temporary directory, unique to this test.
pub fn tmp_dir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "fedifetcher-test-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A config that is valid except for pointing at the given server, with the
/// state kept in the given directory.
pub fn test_config(dir: &Path, server: &str) -> Config {
    Config {
        server: server.to_string(),
        access_tokens: vec!["token".to_string()],
        state_dir: dir.to_path_buf(),
        ..Default::default()
    }
}

/// Open the database in the config's state directory, creating the schema.
pub async fn test_db(config: &Config) -> sqlx::SqlitePool {
    fedifetcher::db::open(config).await.unwrap()
}

/// A run context over the given pieces, for tests that call the fetch
/// functions directly.
pub fn run_context<'a>(
    config: &'a Config,
    state: &'a mut State,
    http: &'a HttpClient,
    home: &'a HomeServer,
    db: &'a sqlx::SqlitePool,
) -> Context<'a> {
    Context {
        config,
        state,
        http,
        home,
        cancel: CancelToken::new(),
        db,
    }
}

/// Captures tracing output, for tests that assert on what gets logged.
///
/// One subscriber is installed globally per test binary; a capturing test
/// registers its buffer under its own thread's id, and events are routed to
/// the buffer of the thread they were recorded on. (A scoped `set_default`
/// subscriber does not work here: tracing's callsite interest cache is
/// global, and parallel tests' subscribers churn it, which can make a
/// later event on this thread miss the subscriber. The global subscriber is
/// installed once and never churns.)
#[derive(Clone, Default)]
pub struct LogCapture {
    buf: std::sync::Arc<std::sync::Mutex<Vec<u8>>>,
}

impl LogCapture {
    pub fn new() -> Self {
        Self::default()
    }

    /// Start capturing this thread's log output; the returned guard stops it
    /// when dropped.
    pub fn install(&self) -> LogCaptureGuard {
        install_capture_subscriber();
        captures()
            .lock()
            .unwrap()
            .insert(std::thread::current().id(), self.clone());
        LogCaptureGuard
    }

    /// The captured text, one line per log event.
    pub fn text(&self) -> String {
        String::from_utf8(self.buf.lock().unwrap().clone()).unwrap()
    }
}

/// Stops the capture that `LogCapture::install` started.
pub struct LogCaptureGuard;

impl Drop for LogCaptureGuard {
    fn drop(&mut self) {
        captures()
            .lock()
            .unwrap()
            .remove(&std::thread::current().id());
    }
}

type Captures = std::sync::Mutex<std::collections::HashMap<std::thread::ThreadId, LogCapture>>;

fn captures() -> &'static Captures {
    static CAPTURES: std::sync::OnceLock<Captures> = std::sync::OnceLock::new();
    CAPTURES.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

fn install_capture_subscriber() {
    static ONCE: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    ONCE.get_or_init(|| {
        // set once per test binary; nothing else installs a subscriber
        let _ = tracing::subscriber::set_global_default(CaptureSubscriber);
    });
}

/// Routes each event to the capturing buffer of the thread it was recorded
/// on; events from threads without a capture are dropped.
struct CaptureSubscriber;

impl tracing::Subscriber for CaptureSubscriber {
    fn enabled(&self, metadata: &tracing::Metadata<'_>) -> bool {
        *metadata.level() <= tracing::Level::INFO
            && captures()
                .lock()
                .unwrap()
                .contains_key(&std::thread::current().id())
    }

    fn new_span(&self, _span: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }

    fn record(&self, _span: &tracing::span::Id, _values: &tracing::span::Record<'_>) {}

    fn record_follows_from(&self, _span: &tracing::span::Id, _follows: &tracing::span::Id) {}

    fn event(&self, event: &tracing::Event<'_>) {
        let capture = captures()
            .lock()
            .unwrap()
            .get(&std::thread::current().id())
            .cloned();
        let Some(capture) = capture else {
            return;
        };
        let mut visitor = MessageVisitor::default();
        event.record(&mut visitor);
        let mut buf = capture.buf.lock().unwrap();
        buf.extend_from_slice(visitor.message.as_bytes());
        buf.push(b'\n');
    }

    fn enter(&self, _span: &tracing::span::Id) {}

    fn exit(&self, _span: &tracing::span::Id) {}

    fn register_callsite(
        &self,
        _metadata: &'static tracing::Metadata<'static>,
    ) -> tracing::subscriber::Interest {
        tracing::subscriber::Interest::always()
    }

    fn max_level_hint(&self) -> Option<tracing::level_filters::LevelFilter> {
        Some(tracing::level_filters::LevelFilter::INFO)
    }
}

/// The `message` field of an event.
#[derive(Default)]
struct MessageVisitor {
    message: String,
}

impl tracing::field::Visit for MessageVisitor {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.message = format!("{value:?}");
        }
    }
}
