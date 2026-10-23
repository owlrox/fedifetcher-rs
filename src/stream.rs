//! Streaming the home timeline: a long-lived connection to our server's
//! event stream, so a new post's context is fetched as it arrives instead
//! of at the next run.
//!
//! The listener runs in `serve` mode, gated by the `streaming` option, with
//! the first access token. Every `update` event is a home timeline post,
//! handled exactly as the home timeline scan handles one: recorded as a
//! trigger post, and its context pulled. The lock file keeps the handling
//! from overlapping a run; an event that arrives while a run holds the lock
//! waits in a small queue, and is handled as soon as the lock comes free.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures::StreamExt;
use serde_json::Value;
use sqlx::SqlitePool;
use tokio::time::sleep;
use tracing::{debug, info, warn};

use crate::api::mastodon::{masked, parse_post, report_mastodon_error, HomeServer};
use crate::config::Config;
use crate::db;
use crate::error::Error;
use crate::http::HttpClient;
use crate::posts::Post;
use crate::run::{CancelToken, Context, Pause};
use crate::store::{lock, LockGuard, StateStore};
use crate::tasks::fetch_timeline_context;
use crate::timeutil;

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

/// How many update events may wait for the lock a run holds. The latest
/// are kept: the ones dropped off the front are the next run's home
/// timeline scan's to find.
const QUEUED_UPDATES: usize = 10;

/// How often the drainer checks for queued events and for the lock coming
/// free.
const QUEUE_POLL: Duration = Duration::from_secs(1);

/// How many drainer polls between lock attempts while the lock file
/// exists. The attempt is what reclaims a lock a dead process left behind,
/// and every failed attempt logs, so the frequent check is the file's
/// existence alone.
const STALE_LOCK_POLLS: u32 = 60;

/// The update events waiting for the lock a run holds: the read loop pushes
/// as events arrive, the drainer task takes them all once it has the lock.
struct UpdateQueue {
    posts: Mutex<VecDeque<Post>>,
}

impl UpdateQueue {
    fn new() -> Self {
        UpdateQueue {
            posts: Mutex::new(VecDeque::new()),
        }
    }

    /// Queue a post, unless it is already waiting. When the queue is full
    /// the oldest waiting post makes room: the latest events are the ones
    /// worth handling first.
    fn push(&self, post: Post) {
        let mut posts = self.posts.lock().unwrap();
        if posts.iter().any(|waiting| waiting.url == post.url) {
            return;
        }
        if posts.len() >= QUEUED_UPDATES {
            posts.pop_front();
        }
        posts.push_back(post);
    }

    fn is_empty(&self) -> bool {
        self.posts.lock().unwrap().is_empty()
    }

    /// Take every waiting post.
    fn take_all(&self) -> Vec<Post> {
        self.posts.lock().unwrap().drain(..).collect()
    }
}

/// What handling an update event needs: the settings, the state database,
/// who to ask, and the queue events wait in while a run holds the lock.
#[derive(Clone)]
struct Handler {
    config: Config,
    db: SqlitePool,
    http: HttpClient,
    home: HomeServer,
    queue: Arc<UpdateQueue>,
}

impl Handler {
    fn new(config: &Config, db: &SqlitePool, scheme: &str, token: &str) -> Self {
        let http = HttpClient::new(config.clone(), db.clone());
        let home = HomeServer::with_scheme(&config.server, scheme, token, http.clone());
        Handler {
            config: config.clone(),
            db: db.clone(),
            http,
            home,
            queue: Arc::new(UpdateQueue::new()),
        }
    }
}

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
    let handler = Handler::new(&config, &db, scheme, &token);
    let base = format!("{scheme}://{}", config.server);
    let stream_base = streaming_base(&base, &handler.http).await;
    let url = format!("{stream_base}/api/v1/streaming/user");
    info!("Streaming home timeline updates from {url}");

    // events that arrive while a run holds the lock wait in the handler's
    // queue, and are handled as soon as it comes free
    tokio::spawn(drain_queued(handler.clone(), cancel.clone()));

    let mut failures = 0usize;
    loop {
        if cancel.is_canceled() {
            debug!("The stream listener stopped");
            return;
        }
        let connected = Instant::now();
        match read_stream(&url, &token, &cancel, &handler).await {
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
async fn read_stream(url: &str, token: &str, cancel: &CancelToken, handler: &Handler) -> StreamEnd {
    let response = match handler.http.authenticated(token).get_stream(url).await {
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
                handle_update(handler, &event.data).await;
            }
        }
    }
}

/// Handle one `update` event: the post is a trigger post, exactly as the
/// home timeline scan treats one — recorded, and its context pulled. The
/// lock keeps the handling from overlapping a run; while one holds it, the
/// post waits in the queue, and the drainer task handles it as soon as the
/// lock comes free.
async fn handle_update(handler: &Handler, data: &str) {
    let Ok(raw) = serde_json::from_str::<Value>(data) else {
        debug!("Stream: an update event whose payload is not JSON; ignored");
        return;
    };
    let Some(post) = parse_post(&raw) else {
        debug!("Stream: an update event whose payload is not a post; ignored");
        return;
    };
    debug!("Stream: update {}", post.url);

    match lock(&handler.config) {
        Ok(lock) => {
            handle_posts(handler, vec![post], lock).await;
        }
        Err(ex) => {
            debug!("Stream: queueing {}: {ex}", post.url);
            handler.queue.push(post);
        }
    }
}

/// Fetch the given posts' context in one session, with the lock held: the
/// posts are recorded as trigger posts under the "stream" source, and the
/// state is written back before the lock is released. The fetch records
/// itself in the run history, like a manual one, so the web UI's pill shows
/// it as running while it holds the lock.
async fn handle_posts(handler: &Handler, posts: Vec<Post>, lock: LockGuard) {
    let started = timeutil::now();
    let run_id = match db::start_run(&handler.db, &timeutil::fmt_state_ts(started)).await {
        Ok(id) => Some(id),
        Err(ex) => {
            warn!("Stream: could not record the fetch's start in the database: {ex}");
            None
        }
    };
    let store = StateStore::new(handler.config.clone());
    let mut session = match store.session(&handler.db).await {
        Ok(session) => session,
        Err(ex) => {
            warn!("Stream: could not load the state: {ex}");
            finish_stream_fetch(
                &handler.db,
                run_id,
                "error",
                &format!("Stream: home timeline (could not load the state: {ex})"),
            )
            .await;
            return;
        }
    };
    {
        let mut ctx = Context {
            config: &handler.config,
            state: session.state(),
            http: &handler.http,
            home: &handler.home,
            cancel: CancelToken::new(),
            pause: Pause::new(),
            db: &handler.db,
        };
        fetch_timeline_context(&mut ctx, posts, "stream").await;
    }
    let (outcome, message) = match session.finish().await {
        Ok(()) => ("ok".to_string(), "Stream: home timeline".to_string()),
        Err(ex) => {
            warn!("Stream: could not write the state back: {ex}");
            (
                "error".to_string(),
                format!("Stream: home timeline (could not write the state back: {ex})"),
            )
        }
    };
    finish_stream_fetch(&handler.db, run_id, &outcome, &message).await;
    drop(lock);
}

/// Complete the stream fetch's run history row.
async fn finish_stream_fetch(db: &SqlitePool, run_id: Option<i64>, outcome: &str, message: &str) {
    let Some(id) = run_id else {
        return;
    };
    if let Err(ex) = db::finish_run(
        db,
        id,
        &timeutil::fmt_state_ts(timeutil::now()),
        outcome,
        message,
    )
    .await
    {
        warn!("Stream: could not record the fetch's end in the database: {ex}");
    }
}

/// Handle the queued update events as soon as the lock a run holds comes
/// free: wait the run out, then fetch them in one session. Runs until the
/// listener is canceled.
async fn drain_queued(handler: Handler, cancel: CancelToken) {
    let mut polls = 0u32;
    loop {
        if cancel.is_canceled() {
            debug!("The stream queue drainer stopped");
            return;
        }
        if !handler.queue.is_empty() {
            polls += 1;
            // the lock file's existence is the cheap contention check; a
            // real attempt is made when it is gone, and every
            // STALE_LOCK_POLLS polls regardless, because the attempt is
            // what reclaims a lock a dead process left behind (and every
            // failed attempt logs)
            if !handler.config.lock_path().exists() || polls.is_multiple_of(STALE_LOCK_POLLS) {
                if let Ok(lock) = lock(&handler.config) {
                    let posts = handler.queue.take_all();
                    if posts.is_empty() {
                        drop(lock);
                    } else {
                        debug!("Stream: handling {} queued update(s)", posts.len());
                        handle_posts(&handler, posts, lock).await;
                    }
                    polls = 0;
                }
            }
        }
        sleep(QUEUE_POLL).await;
    }
}

/// The base URL the event stream is served from: the streaming host the
/// instance's v2 instance document names — a WebSocket URL, mapped to its
/// HTTP equivalent — or the API's own host when the document says nothing,
/// cannot be read, or names nothing recognizable.
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
        Some(streaming) => http_base(streaming).unwrap_or_else(|| base.to_string()),
        _ => base.to_string(),
    }
}

/// The HTTP base of the event stream: the instance document names the
/// streaming host as a WebSocket URL, and the stream here is read over
/// HTTP, so the scheme is mapped — `wss` to `https`, `ws` to `http` — and
/// an HTTP URL is taken as it is. Nothing recognizable gives None, and the
/// caller falls back to the API's own host.
fn http_base(streaming: &str) -> Option<String> {
    let (scheme, host) = if let Some(host) = streaming.strip_prefix("wss://") {
        ("https", host)
    } else if let Some(host) = streaming.strip_prefix("ws://") {
        ("http", host)
    } else if let Some(host) = streaming.strip_prefix("https://") {
        ("https", host)
    } else {
        ("http", streaming.strip_prefix("http://")?)
    };
    let host = host.trim_end_matches('/');
    if host.is_empty() {
        return None;
    }
    Some(format!("{scheme}://{host}"))
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

    fn post(url: &str) -> Post {
        parse_post(&serde_json::json!({
            "id": "1",
            "url": url,
            "uri": url,
            "created_at": "2026-10-23T10:00:00.000Z",
            "visibility": "public",
            "reblog": null,
            "account": {"acct": "alice@example.com", "url": "https://example.com/@alice"},
        }))
        .expect("a post")
    }

    #[test]
    fn the_queue_takes_every_waiting_post() {
        let queue = UpdateQueue::new();
        assert!(queue.is_empty());
        queue.push(post("https://example.com/@alice/1"));
        queue.push(post("https://example.com/@alice/2"));
        let posts = queue.take_all();
        assert_eq!(posts.len(), 2);
        assert_eq!(posts[0].url, "https://example.com/@alice/1");
        assert!(queue.is_empty(), "take_all empties the queue");
    }

    #[test]
    fn a_post_already_waiting_is_not_queued_twice() {
        let queue = UpdateQueue::new();
        queue.push(post("https://example.com/@alice/1"));
        queue.push(post("https://example.com/@alice/1"));
        assert_eq!(queue.take_all().len(), 1);
    }

    #[test]
    fn a_full_queue_keeps_the_latest_posts() {
        let queue = UpdateQueue::new();
        for n in 0..QUEUED_UPDATES + 3 {
            queue.push(post(&format!("https://example.com/@alice/{n}")));
        }
        let posts = queue.take_all();
        assert_eq!(posts.len(), QUEUED_UPDATES);
        assert_eq!(
            posts[0].url, "https://example.com/@alice/3",
            "the oldest waiting posts made room"
        );
        assert_eq!(
            posts[QUEUED_UPDATES - 1].url,
            format!("https://example.com/@alice/{}", QUEUED_UPDATES + 2)
        );
    }

    #[tokio::test]
    async fn the_drainer_stops_when_canceled() {
        let config = Config {
            server: "home.example".to_string(),
            access_tokens: vec!["t".to_string()],
            ..Default::default()
        };
        let db = crate::db::memory_pool();
        let handler = Handler::new(&config, &db, "https", "t");
        let cancel = CancelToken::new();
        cancel.cancel();
        let drainer = tokio::spawn(drain_queued(handler, cancel));
        tokio::time::timeout(std::time::Duration::from_secs(5), drainer)
            .await
            .expect("the canceled drainer stops promptly")
            .expect("the drainer did not panic");
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

    #[test]
    fn a_websocket_streaming_url_maps_to_http() {
        assert_eq!(
            http_base("wss://stream.example.com"),
            Some("https://stream.example.com".to_string())
        );
        assert_eq!(
            http_base("wss://stream.example.com/"),
            Some("https://stream.example.com".to_string()),
            "the trailing slash is trimmed"
        );
        assert_eq!(
            http_base("ws://stream.example.com:4000"),
            Some("http://stream.example.com:4000".to_string())
        );
    }

    #[test]
    fn an_http_streaming_url_is_taken_as_it_is() {
        assert_eq!(
            http_base("https://stream.example.com"),
            Some("https://stream.example.com".to_string())
        );
        assert_eq!(
            http_base("http://stream.example.com"),
            Some("http://stream.example.com".to_string())
        );
    }

    #[test]
    fn an_unrecognizable_streaming_url_gives_none() {
        assert_eq!(http_base("ftp://stream.example.com"), None);
        assert_eq!(http_base("wss://"), None, "no host");
        assert_eq!(http_base(""), None);
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
