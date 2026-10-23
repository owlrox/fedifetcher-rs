//! Streaming the home timeline: a long-lived connection to our server's
//! event stream, so a new post's context is fetched as it arrives instead
//! of at the next run.
//!
//! The listener runs in `serve` mode, gated by the `streaming` option, with
//! the first access token. Every `update` event is a home timeline post,
//! handled exactly as the home timeline scan handles one: recorded as a
//! trigger post, and its context pulled. The lock file keeps the handling
//! from overlapping a run.

use std::time::{Duration, Instant};

use futures::StreamExt;
use serde_json::Value;
use sqlx::SqlitePool;
use tokio::time::sleep;
use tracing::{debug, info, warn};

use crate::api::mastodon::{masked, parse_post, report_mastodon_error, HomeServer};
use crate::config::Config;
use crate::error::Error;
use crate::http::HttpClient;
use crate::run::{CancelToken, Context, Pause};
use crate::store::{lock, StateStore};
use crate::tasks::fetch_timeline_context;

/// Seconds to wait before reconnecting after the stream ends or fails,
/// tried in order, the last one repeating.
const RECONNECT_DELAYS: [u64; 5] = [1, 5, 15, 60, 300];

/// How long the stream may sit silent before the connection is treated as
/// dead and re-established. The server's keepalive comments arrive well
/// inside this.
const READ_TIMEOUT: Duration = Duration::from_secs(300);

/// How often a blocked stream read re-checks the cancel flag, and how the
/// silence clock ticks.
const READ_POLL: Duration = Duration::from_secs(1);

/// How long a connection must last before the reconnect delay starts over:
/// a stream that ran for a while ended for a reason worth forgetting.
const STABLE_CONNECTION: Duration = Duration::from_secs(60);

/// How often the reconnect wait re-checks the cancel flag.
const CANCEL_POLL: Duration = Duration::from_millis(200);

/// One server-sent event: its `event:` name, empty for the default
/// "message", and its `data:` lines joined with newlines.
struct StreamEvent {
    event: String,
    data: String,
}

/// An incremental parser for server-sent events: bytes go in, the events
/// they complete come out. Lines end at `\n`, `\r\n` or `\r`; a blank line
/// dispatches the event accumulated so far; a line starting with `:` is a
/// comment — the server's keepalive — and is ignored.
struct EventStreamParser {
    buffer: Vec<u8>,
    event: String,
    data: String,
}

impl EventStreamParser {
    fn new() -> Self {
        EventStreamParser {
            buffer: Vec::new(),
            event: String::new(),
            data: String::new(),
        }
    }

    /// Feed bytes into the parser, returning the events they completed.
    fn feed(&mut self, bytes: &[u8]) -> Vec<StreamEvent> {
        self.buffer.extend_from_slice(bytes);
        let mut events = Vec::new();
        while let Some(line) = take_line(&mut self.buffer) {
            if line.is_empty() {
                // a blank line dispatches the event; one with no data is
                // not dispatched at all
                if !self.data.is_empty() {
                    events.push(StreamEvent {
                        event: std::mem::take(&mut self.event),
                        data: std::mem::take(&mut self.data),
                    });
                } else {
                    self.event.clear();
                }
                continue;
            }
            // a comment: the server's keepalive
            if line.starts_with(b":") {
                continue;
            }
            let Some((field, value)) = split_field(&line) else {
                continue;
            };
            let Ok(field) = std::str::from_utf8(field) else {
                continue;
            };
            match field {
                "event" => self.event = String::from_utf8_lossy(value).into_owned(),
                "data" => {
                    if !self.data.is_empty() {
                        self.data.push('\n');
                    }
                    self.data.push_str(&String::from_utf8_lossy(value));
                }
                // id and retry: not used
                _ => {}
            }
        }
        events
    }
}

/// Take one line off the front of the buffer, without its terminator.
/// Returns `None` when the buffer holds no complete line yet — including
/// when it ends at a `\r` whose `\n` may still be on its way.
fn take_line(buffer: &mut Vec<u8>) -> Option<Vec<u8>> {
    let end = buffer
        .iter()
        .position(|byte| *byte == b'\n' || *byte == b'\r')?;
    // a `\r` as the last byte might be half of a `\r\n`; wait for more
    if buffer[end] == b'\r' && end == buffer.len() - 1 {
        return None;
    }
    let line: Vec<u8> = buffer.drain(..end).collect();
    match buffer.first() {
        Some(b'\n') => {
            buffer.drain(..1);
        }
        Some(b'\r') => {
            buffer.drain(..1);
            if buffer.first() == Some(&b'\n') {
                buffer.drain(..1);
            }
        }
        _ => {}
    }
    Some(line)
}

/// Split a line into its field and value at the first colon, with the one
/// space after the colon gone. A line with no colon is a field with an
/// empty value, which nothing here cares about.
fn split_field(line: &[u8]) -> Option<(&[u8], &[u8])> {
    let colon = line.iter().position(|byte| *byte == b':')?;
    let value = &line[colon + 1..];
    Some((&line[..colon], value.strip_prefix(b" ").unwrap_or(value)))
}

/// Listen to the home timeline stream until `cancel` is set. Each `update`
/// event is handled as it arrives; a stream that ends or fails is
/// reconnected after a growing delay. The config is the one at startup:
/// changing it applies on restart.
pub async fn listen(config: Config, db: SqlitePool, cancel: CancelToken) {
    listen_with_scheme("https", config, db, cancel).await
}

/// A listener for the API at `scheme://<config.server>`. Production always
/// uses `listen` (https); tests point this at a local http server.
pub async fn listen_with_scheme(scheme: &str, config: Config, db: SqlitePool, cancel: CancelToken) {
    let Some(token) = config.access_tokens.first().cloned() else {
        warn!("streaming is enabled but no access token is configured; not streaming");
        return;
    };
    let http = HttpClient::new(config.clone(), db.clone());
    let home = HomeServer::with_scheme(&config.server, scheme, &token, http.clone());
    let base = format!("{scheme}://{}", config.server);
    let stream_base = streaming_base(&base, &http).await;
    let url = format!("{stream_base}/api/v1/streaming/user");
    info!("Streaming home timeline updates from {url}");

    let mut failures = 0usize;
    loop {
        if cancel.is_canceled() {
            debug!("The stream listener stopped");
            return;
        }
        let connected = Instant::now();
        match read_stream(&url, &token, &http, &cancel, &config, &db, &home).await {
            StreamEnd::Closed => debug!("The stream ended; reconnecting"),
            StreamEnd::Refused => {
                // the token was refused; no retry would change that
                return;
            }
            StreamEnd::Failed(ex) => warn!("The stream failed: {ex}; reconnecting"),
        }
        // a connection that lasted a while ended for a reason worth
        // forgetting; a quick failure waits longer next time
        if connected.elapsed() >= STABLE_CONNECTION {
            failures = 0;
        }
        let delay = RECONNECT_DELAYS[failures.min(RECONNECT_DELAYS.len() - 1)];
        failures = (failures + 1).min(RECONNECT_DELAYS.len() - 1);
        wait_out(delay, &cancel).await;
    }
}

/// Why reading the stream stopped.
enum StreamEnd {
    /// The server closed the connection, it went silent past the read
    /// timeout, or the listener was canceled.
    Closed,
    /// The server refused the access token: a retry would not change that.
    Refused,
    /// The connection or the reading failed.
    Failed(Error),
}

/// Connect to the stream and handle its events until it ends, goes silent,
/// or the listener is canceled; returns why the reading stopped.
async fn read_stream(
    url: &str,
    token: &str,
    http: &HttpClient,
    cancel: &CancelToken,
    config: &Config,
    db: &SqlitePool,
    home: &HomeServer,
) -> StreamEnd {
    let response = match http.authenticated(token).get_stream(url).await {
        Ok(response) => response,
        Err(ex) => return StreamEnd::Failed(ex),
    };
    let status = response.status().as_u16();
    if status == 401 || status == 403 {
        warn!(
            "{}",
            report_mastodon_error(
                &format!("Error connecting to the stream {url}"),
                status,
                "read:statuses",
                Some(&masked(token)),
            )
        );
        return StreamEnd::Refused;
    }
    if status != 200 {
        return StreamEnd::Failed(Error::Fetch(format!(
            "Error connecting to the stream {url}. Status code: {status}"
        )));
    }

    let mut parser = EventStreamParser::new();
    let mut chunks = response.bytes_stream();
    let mut silent = Duration::ZERO;
    loop {
        if cancel.is_canceled() {
            return StreamEnd::Closed;
        }
        let chunk = match tokio::time::timeout(READ_POLL, chunks.next()).await {
            Ok(Some(Ok(chunk))) => {
                silent = Duration::ZERO;
                chunk
            }
            Ok(Some(Err(ex))) => {
                return StreamEnd::Failed(Error::HttpRead {
                    url: url.to_string(),
                    source: ex,
                })
            }
            // the server closed the stream
            Ok(None) => return StreamEnd::Closed,
            // nothing arrived: count the silence, and give up on the
            // connection once it passes the read timeout
            Err(_) => {
                silent += READ_POLL;
                if silent >= READ_TIMEOUT {
                    debug!("The stream went silent; reconnecting");
                    return StreamEnd::Closed;
                }
                continue;
            }
        };
        for event in parser.feed(&chunk) {
            if cancel.is_canceled() {
                return StreamEnd::Closed;
            }
            if event.event == "update" {
                handle_update(config, db, http, home, &event.data).await;
            }
        }
    }
}

/// Handle one `update` event: the post is a trigger post, exactly as the
/// home timeline scan treats one — recorded, and its context pulled. The
/// lock keeps the handling from overlapping a run; when one holds it, the
/// event is skipped, because that run is scanning and will find the post.
async fn handle_update(
    config: &Config,
    db: &SqlitePool,
    http: &HttpClient,
    home: &HomeServer,
    data: &str,
) {
    let Ok(raw) = serde_json::from_str::<Value>(data) else {
        debug!("Stream: an update event whose payload is not JSON; ignored");
        return;
    };
    let Some(post) = parse_post(&raw) else {
        debug!("Stream: an update event whose payload is not a post; ignored");
        return;
    };
    debug!("Stream: update {}", post.url);

    let lock = match lock(config) {
        Ok(lock) => lock,
        Err(ex) => {
            debug!("Stream: not handling {} now: {ex}", post.url);
            return;
        }
    };

    let store = StateStore::new(config.clone());
    let mut session = match store.session(db).await {
        Ok(session) => session,
        Err(ex) => {
            warn!("Stream: could not load the state for {}: {ex}", post.url);
            return;
        }
    };
    {
        let mut ctx = Context {
            config,
            state: session.state(),
            http,
            home,
            cancel: CancelToken::new(),
            pause: Pause::new(),
            db,
        };
        fetch_timeline_context(&mut ctx, vec![post], "stream").await;
    }
    if let Err(ex) = session.finish().await {
        warn!("Stream: could not write the state back: {ex}");
    }
    drop(lock);
}

/// The base URL the event stream is served from: the streaming host the
/// instance's v2 instance document names, or the API's own host when the
/// document says nothing or cannot be read.
async fn streaming_base(base: &str, http: &HttpClient) -> String {
    let url = format!("{base}/api/v2/instance");
    let response = match http.ignoring_robots().get(&url, None).await {
        Ok(response) if response.status == 200 => response,
        Ok(response) => {
            debug!(
                "The instance document answered {}; streaming from {base}",
                response.status
            );
            return base.to_string();
        }
        Err(ex) => {
            debug!("Could not read the instance document ({ex}); streaming from {base}");
            return base.to_string();
        }
    };
    let Ok(instance) = response.json() else {
        return base.to_string();
    };
    match instance
        .pointer("/configuration/urls/streaming")
        .and_then(Value::as_str)
    {
        Some(streaming) if !streaming.is_empty() => streaming.trim_end_matches('/').to_string(),
        _ => base.to_string(),
    }
}

/// Wait out the reconnect delay, re-checking the cancel flag as it goes.
async fn wait_out(seconds: u64, cancel: &CancelToken) {
    let delay = Duration::from_secs(seconds);
    let mut waited = Duration::ZERO;
    while waited < delay && !cancel.is_canceled() {
        sleep(CANCEL_POLL).await;
        waited += CANCEL_POLL;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn events(feeds: &[&[u8]]) -> Vec<StreamEvent> {
        let mut parser = EventStreamParser::new();
        let mut events = Vec::new();
        for feed in feeds {
            events.extend(parser.feed(feed));
        }
        events
    }

    #[test]
    fn an_event_is_dispatched_at_the_blank_line() {
        let events = events(&[b"event: update\ndata: {\"id\":\"1\"}\n\n"]);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event, "update");
        assert_eq!(events[0].data, "{\"id\":\"1\"}");
    }

    #[test]
    fn data_lines_join_with_newlines() {
        let events = events(&[b"event: update\ndata: one\ndata: two\n\n"]);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].data, "one\ntwo");
    }

    #[test]
    fn the_space_after_the_colon_is_not_part_of_the_value() {
        let events = events(&[b"event:update\ndata:{\"a\":1}\n\n"]);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event, "update");
        assert_eq!(events[0].data, "{\"a\":1}");
    }

    #[test]
    fn comments_are_ignored() {
        let events = events(&[b":ping\n\n", b"event: update\ndata: x\n\n"]);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event, "update");
    }

    #[test]
    fn an_event_without_data_is_not_dispatched() {
        assert!(events(&[b"event: update\n\n"]).is_empty());
    }

    #[test]
    fn an_event_without_a_name_is_a_message() {
        let events = events(&[b"data: hello\n\n"]);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event, "");
        assert_eq!(events[0].data, "hello");
    }

    #[test]
    fn events_split_across_chunks_are_completed() {
        let events = events(&[b"event: up", b"date\ndata: {\"a\"", b": 1}\n", b"\n"]);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event, "update");
        assert_eq!(events[0].data, "{\"a\": 1}");
    }

    #[test]
    fn crlf_line_endings_are_read() {
        let events = events(&[b"event: update\r\ndata: x\r\n\r\n"]);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event, "update");
        assert_eq!(events[0].data, "x");
    }

    #[test]
    fn a_cr_split_from_its_lf_waits_for_more() {
        // the `\r` might be half of a `\r\n`; it is not a line ending yet
        let mut parser = EventStreamParser::new();
        assert!(parser.feed(b"data: x\r").is_empty());
        let events = parser.feed(b"\n\n");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].data, "x");
    }

    #[test]
    fn several_events_in_one_chunk_come_out_in_order() {
        let events = events(&[b"event: update\ndata: 1\n\nevent: notification\ndata: 2\n\n"]);
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].event, "update");
        assert_eq!(events[0].data, "1");
        assert_eq!(events[1].event, "notification");
    }

    #[test]
    fn fields_nobody_uses_are_ignored() {
        let events = events(&[b"id: 42\nretry: 1000\nevent: update\ndata: x\n\n"]);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].data, "x");
    }

    fn http() -> HttpClient {
        HttpClient::new(
            Config {
                server: "home.example".to_string(),
                access_tokens: vec!["t".to_string()],
                ..Default::default()
            },
            crate::db::memory_pool(),
        )
    }

    #[tokio::test]
    async fn the_streaming_host_comes_from_the_instance_document() {
        let server = httpmock::MockServer::start_async().await;
        server
            .mock_async(|when, then| {
                when.method(httpmock::Method::GET).path("/api/v2/instance");
                then.status(200).json_body(serde_json::json!({
                    "configuration": {"urls": {"streaming": "https://stream.example.com/"}}
                }));
            })
            .await;
        assert_eq!(
            streaming_base(&server.base_url(), &http()).await,
            "https://stream.example.com",
            "the trailing slash is trimmed"
        );
    }

    #[tokio::test]
    async fn the_streaming_host_defaults_to_the_api_host() {
        let server = httpmock::MockServer::start_async().await;
        server
            .mock_async(|when, then| {
                when.method(httpmock::Method::GET).path("/api/v2/instance");
                then.status(200)
                    .json_body(serde_json::json!({"configuration": {"urls": {}}}));
            })
            .await;
        assert_eq!(
            streaming_base(&server.base_url(), &http()).await,
            server.base_url()
        );
    }

    #[tokio::test]
    async fn a_missing_instance_document_streams_from_the_api_host() {
        let server = httpmock::MockServer::start_async().await;
        server
            .mock_async(|when, then| {
                when.method(httpmock::Method::GET).path("/api/v2/instance");
                then.status(404);
            })
            .await;
        assert_eq!(
            streaming_base(&server.base_url(), &http()).await,
            server.base_url()
        );
    }
}
