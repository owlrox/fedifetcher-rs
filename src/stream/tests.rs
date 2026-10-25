//! The tests for `stream.rs`. Kept in a child module so they see the
//! private items; the parent holds only the `mod tests;` pointer.

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
