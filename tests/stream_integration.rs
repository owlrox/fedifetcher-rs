//! The home timeline stream: a long-lived connection that turns `update`
//! events into immediate trigger posts, reconnecting when the server closes
//! it. The stream is spoken by hand over a raw socket, because httpmock
//! answers with a whole body, and a stream never ends.

mod common;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use fedifetcher::db;
use fedifetcher::run::CancelToken;
use fedifetcher::stream;
use httpmock::prelude::*;
use httpmock::Method;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// The mock server's host, without the scheme.
fn host(server: &MockServer) -> String {
    server.base_url().trim_start_matches("http://").to_string()
}

/// A server that speaks server-sent events by hand: it answers every
/// connection with the given events, then closes it, so the listener has to
/// reconnect — and reconnect it does. Returns the server's base URL and a
/// count of the connections it has answered.
async fn sse_server(events: &[String]) -> (String, Arc<AtomicUsize>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let connections = Arc::new(AtomicUsize::new(0));
    let events = events.to_vec();
    let counted = connections.clone();
    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            counted.fetch_add(1, Ordering::Relaxed);
            let events = events.clone();
            tokio::spawn(async move {
                // read the request up to the blank line ending its headers
                let mut request = Vec::new();
                let mut byte = [0u8; 1];
                while !request.ends_with(b"\r\n\r\n") && !request.ends_with(b"\n\n") {
                    match socket.read(&mut byte).await {
                        Ok(0) | Err(_) => return,
                        Ok(_) => request.push(byte[0]),
                    }
                }
                let head = "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncache-control: no-cache\r\n\r\n";
                if socket.write_all(head.as_bytes()).await.is_err() {
                    return;
                }
                for event in &events {
                    if socket.write_all(event.as_bytes()).await.is_err() {
                        return;
                    }
                }
                // dropping the socket ends the stream; the listener reconnects
            });
        }
    });
    (format!("http://{addr}"), connections)
}

/// One server-sent event carrying a post with the given visibility. The
/// post's author lives on an unreachable host, so its context can never be
/// fetched — what the tests assert is what the event does locally.
fn post_event(name: &str, url: &str, visibility: &str) -> String {
    format!(
        "event: {name}\ndata: {{\"id\":\"1\",\"url\":\"{url}\",\"uri\":\"{url}\",\
         \"created_at\":\"2026-10-23T10:00:00.000Z\",\"visibility\":\"{visibility}\",\
         \"reblog\":null,\"account\":{{\"acct\":\"alice@127.0.0.1:9\",\
         \"url\":\"https://127.0.0.1:9/@alice\"}}}}\n\n"
    )
}

/// The home server mock whose instance document names the streaming host.
async fn home_with_stream(server: &MockServer, stream_url: &str) {
    server
        .mock_async(|when, then| {
            when.method(Method::GET).path("/api/v2/instance");
            then.status(200).json_body(serde_json::json!({
                "configuration": {"urls": {"streaming": stream_url}}
            }));
        })
        .await;
}

/// Start the stream listener in the background, returning its task and the
/// token that stops it.
fn start_listener(
    config: fedifetcher::config::Config,
    db: sqlx::SqlitePool,
) -> (tokio::task::JoinHandle<()>, CancelToken) {
    let cancel = CancelToken::new();
    let task = {
        let (config, db, cancel) = (config, db, cancel.clone());
        tokio::spawn(async move { stream::listen_with_scheme("http", config, db, cancel).await })
    };
    (task, cancel)
}

/// Wait until `condition` holds, polling as the web tests do.
async fn until<F, Fut>(mut condition: F)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    for _ in 0..200 {
        if condition().await {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
}

/// A streamed home timeline post becomes a trigger post, recorded with the
/// stream as its source; events that are not updates are ignored; and the
/// listener reconnects after the server closes the stream.
#[tokio::test]
async fn a_streamed_update_becomes_a_trigger_post() {
    let dir = common::tmp_dir();
    let home = MockServer::start_async().await;
    let (stream_url, connections) = sse_server(&[
        post_event("notification", "https://127.0.0.1:9/@alice/9", "public"),
        post_event("update", "https://127.0.0.1:9/@alice/1", "public"),
    ])
    .await;
    home_with_stream(&home, &stream_url).await;
    let search = home
        .mock_async(|when, then| {
            when.method(Method::GET).path("/api/v2/search");
            then.status(200);
        })
        .await;

    let mut config = common::test_config(&dir, &host(&home));
    config.streaming = true;
    let db = db::open(&config).await.unwrap();
    let (listener, cancel) = start_listener(config, db.clone());

    // the event is handled in the background: poll for its record, and for
    // the reconnect after the server closed the first connection
    let db2 = db.clone();
    until(|| {
        let db = db2.clone();
        async move {
            db::trigger_post(&db, "https://127.0.0.1:9/@alice/1")
                .await
                .unwrap()
                .is_some()
        }
    })
    .await;
    let connections2 = connections.clone();
    until(|| {
        let connections = connections2.clone();
        async move { connections.load(Ordering::Relaxed) >= 2 }
    })
    .await;
    cancel.cancel();
    tokio::time::timeout(std::time::Duration::from_secs(10), listener)
        .await
        .expect("the canceled listener stops promptly")
        .expect("the listener did not panic");

    let recorded = db::trigger_post(&db, "https://127.0.0.1:9/@alice/1")
        .await
        .unwrap()
        .expect("the streamed post is recorded as a trigger post");
    assert_eq!(recorded.source, "stream");
    assert_eq!(recorded.url, "https://127.0.0.1:9/@alice/1");
    assert!(
        connections.load(Ordering::Relaxed) >= 2,
        "the listener reconnected after the stream ended"
    );
    assert!(
        db::trigger_post(&db, "https://127.0.0.1:9/@alice/9")
            .await
            .unwrap()
            .is_none(),
        "a notification event is not a trigger post"
    );
    assert_eq!(
        search.calls(),
        0,
        "the post's server is unreachable, so no context was found to resolve"
    );
}

/// A streamed post the server reports as private is recorded as private,
/// and its context is never fetched.
#[tokio::test]
async fn a_streamed_private_post_is_recorded_as_private() {
    let dir = common::tmp_dir();
    let home = MockServer::start_async().await;
    let (stream_url, _connections) = sse_server(&[post_event(
        "update",
        "https://127.0.0.1:9/@alice/2",
        "private",
    )])
    .await;
    home_with_stream(&home, &stream_url).await;

    let mut config = common::test_config(&dir, &host(&home));
    config.streaming = true;
    let db = db::open(&config).await.unwrap();
    let (listener, cancel) = start_listener(config, db.clone());

    let db2 = db.clone();
    until(|| {
        let db = db2.clone();
        async move {
            db::trigger_post(&db, "https://127.0.0.1:9/@alice/2")
                .await
                .unwrap()
                .is_some()
                && db::is_private_post(&db, "https://127.0.0.1:9/@alice/2")
                    .await
                    .unwrap()
        }
    })
    .await;
    cancel.cancel();
    tokio::time::timeout(std::time::Duration::from_secs(10), listener)
        .await
        .expect("the canceled listener stops promptly")
        .expect("the listener did not panic");

    let recorded = db::trigger_post(&db, "https://127.0.0.1:9/@alice/2")
        .await
        .unwrap()
        .expect("the streamed post is recorded as a trigger post");
    assert_eq!(recorded.source, "stream");
    assert!(
        db::is_private_post(&db, "https://127.0.0.1:9/@alice/2")
            .await
            .unwrap(),
        "the private post is recorded as private"
    );
}

/// A listener whose server refuses the access token stops instead of
/// retrying: no retry would change a refused token.
#[tokio::test]
async fn a_refused_token_stops_the_listener() {
    let dir = common::tmp_dir();
    let home = MockServer::start_async().await;
    home.mock_async(|when, then| {
        when.method(Method::GET).path("/api/v2/instance");
        then.status(200)
            .json_body(serde_json::json!({"configuration": {"urls": {}}}));
    })
    .await;
    let stream = home
        .mock_async(|when, then| {
            when.method(Method::GET).path("/api/v1/streaming/user");
            then.status(401);
        })
        .await;

    let mut config = common::test_config(&dir, &host(&home));
    config.streaming = true;
    let db = db::open(&config).await.unwrap();
    let (listener, cancel) = start_listener(config, db);

    // the listener gives up on its own: no cancel needed
    tokio::time::timeout(std::time::Duration::from_secs(10), listener)
        .await
        .expect("the listener stops when the token is refused")
        .expect("the listener did not panic");
    drop(cancel);

    assert_eq!(stream.calls(), 1, "the stream was tried once, not retried");
}

/// An update that arrives while a run holds the lock waits in the queue,
/// and is handled as soon as the lock comes free.
#[tokio::test]
async fn an_update_during_a_run_is_queued_and_handled_after_it() {
    let dir = common::tmp_dir();
    let home = MockServer::start_async().await;
    let (stream_url, connections) = sse_server(&[post_event(
        "update",
        "https://127.0.0.1:9/@alice/3",
        "public",
    )])
    .await;
    home_with_stream(&home, &stream_url).await;
    home.mock_async(|when, then| {
        when.method(Method::GET).path("/api/v2/search");
        then.status(200);
    })
    .await;

    let mut config = common::test_config(&dir, &host(&home));
    config.streaming = true;
    let db = db::open(&config).await.unwrap();

    // a run holds the lock: the event is queued, not handled
    let guard = fedifetcher::store::lock(&config).unwrap();
    let (listener, cancel) = start_listener(config, db.clone());
    let connections2 = connections.clone();
    until(|| {
        let connections = connections2.clone();
        async move { connections.load(Ordering::Relaxed) >= 1 }
    })
    .await;
    // the event has been read by now; give the handling a moment to queue it
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    assert!(
        db::trigger_post(&db, "https://127.0.0.1:9/@alice/3")
            .await
            .unwrap()
            .is_none(),
        "the update is not handled while the run holds the lock"
    );

    // the run ends: the queued update is handled
    drop(guard);
    let db2 = db.clone();
    until(|| {
        let db = db2.clone();
        async move {
            db::trigger_post(&db, "https://127.0.0.1:9/@alice/3")
                .await
                .unwrap()
                .is_some()
        }
    })
    .await;
    cancel.cancel();
    tokio::time::timeout(std::time::Duration::from_secs(10), listener)
        .await
        .expect("the canceled listener stops promptly")
        .expect("the listener did not panic");

    let recorded = db::trigger_post(&db, "https://127.0.0.1:9/@alice/3")
        .await
        .unwrap()
        .expect("the queued update was handled once the lock came free");
    assert_eq!(recorded.source, "stream");
}

/// The instance document names the streaming host as a WebSocket URL; the
/// stream is read over HTTP, so the scheme is mapped and the listener
/// connects anyway.
#[tokio::test]
async fn a_websocket_streaming_url_is_read_over_http() {
    let dir = common::tmp_dir();
    let home = MockServer::start_async().await;
    let (stream_url, connections) = sse_server(&[post_event(
        "update",
        "https://127.0.0.1:9/@alice/4",
        "public",
    )])
    .await;
    // the instance document names the stream's host as a WebSocket URL
    home_with_stream(&home, &stream_url.replace("http://", "ws://")).await;
    home.mock_async(|when, then| {
        when.method(Method::GET).path("/api/v2/search");
        then.status(200);
    })
    .await;

    let mut config = common::test_config(&dir, &host(&home));
    config.streaming = true;
    let db = db::open(&config).await.unwrap();
    let (listener, cancel) = start_listener(config, db.clone());

    let db2 = db.clone();
    until(|| {
        let db = db2.clone();
        async move {
            db::trigger_post(&db, "https://127.0.0.1:9/@alice/4")
                .await
                .unwrap()
                .is_some()
        }
    })
    .await;
    cancel.cancel();
    tokio::time::timeout(std::time::Duration::from_secs(10), listener)
        .await
        .expect("the canceled listener stops promptly")
        .expect("the listener did not panic");

    assert!(
        connections.load(Ordering::Relaxed) >= 1,
        "the listener connected to the streaming host over HTTP"
    );
    let recorded = db::trigger_post(&db, "https://127.0.0.1:9/@alice/4")
        .await
        .unwrap()
        .expect("the streamed post is recorded as a trigger post");
    assert_eq!(recorded.source, "stream");
}
