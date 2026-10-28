//! The tests for `http.rs`. Kept in a child module so they see the
//! private items; the parent holds only the `mod tests;` pointer.

use super::*;

#[test]
fn the_user_agent_names_the_server_unless_hidden_or_overridden() {
    let mut config = Config {
        server: "example.social".to_string(),
        ..Default::default()
    };
    assert_eq!(
        user_agent_string(&config),
        format!("FediFetcher/{VERSION}; +example.social (+https://github.com/owlrox/fedifetcher-rs)")
    );
    config.hide_server_in_user_agent = true;
    assert_eq!(
        user_agent_string(&config),
        format!("FediFetcher/{VERSION} (+https://github.com/owlrox/fedifetcher-rs)")
    );
    // the override replaces the whole agent, whatever hide_server says
    config.user_agent_override = Some("MyFetcher/1.0".to_string());
    assert_eq!(user_agent_string(&config), "MyFetcher/1.0");
    config.hide_server_in_user_agent = false;
    assert_eq!(user_agent_string(&config), "MyFetcher/1.0");
    // a blank override never reaches the wire: unset means the default,
    // so the agent is never sent empty — servers refuse or ignore blanks
    config.user_agent_override = Some("   ".to_string());
    assert_eq!(
        user_agent_string(&config),
        format!("FediFetcher/{VERSION}; +example.social (+https://github.com/owlrox/fedifetcher-rs)")
    );
}

#[test]
fn callback_url_replaces_and_adds_params() {
    assert_eq!(
        build_callback_url(
            "https://watch.example/ping?rid=old&keep=1",
            &[
                ("rid".to_string(), "new".to_string()),
                ("msg".to_string(), "hi there".to_string())
            ]
        ),
        "https://watch.example/ping?rid=new&keep=1&msg=hi+there"
    );
    assert_eq!(
        build_callback_url(
            "https://watch.example/ping",
            &[("a".to_string(), "1".to_string())]
        ),
        "https://watch.example/ping?a=1"
    );
    // blank values are kept
    assert_eq!(
        build_callback_url(
            "https://watch.example/ping?empty=",
            &[("a".to_string(), "1".to_string())]
        ),
        "https://watch.example/ping?empty=&a=1"
    );
}

#[test]
fn link_header_parses() {
    let mut headers = HeaderMap::new();
    headers.insert(
            LINK,
            HeaderValue::from_static(
                r#"<https://home.example/api/v1/bookmarks?max_id=42>; rel="next", <https://home.example/api/v1/bookmarks?min_id=99>; rel="prev""#,
            ),
        );
    let response = Response {
        status: 200,
        headers,
        text: String::new(),
    };
    let links = response.links();
    assert_eq!(
        links.get("next"),
        Some(&"https://home.example/api/v1/bookmarks?max_id=42".to_string())
    );
    assert_eq!(
        links.get("prev"),
        Some(&"https://home.example/api/v1/bookmarks?min_id=99".to_string())
    );
}

#[test]
fn link_header_parts_without_a_rel_are_skipped() {
    let mut headers = HeaderMap::new();
    headers.insert(
        LINK,
        HeaderValue::from_static(
            r#"<https://home.example/a>; type="text/html", <https://home.example/b>; rel="next""#,
        ),
    );
    let response = Response {
        status: 200,
        headers,
        text: String::new(),
    };
    let links = response.links();
    assert_eq!(links.len(), 1);
    assert_eq!(
        links.get("next"),
        Some(&"https://home.example/b".to_string())
    );
}

#[test]
fn retry_delay_uses_the_servers_reset_header_when_it_parses() {
    let in_a_minute = timeutil::fmt_state_ts(timeutil::now() + time::Duration::minutes(1));
    let mut headers = HeaderMap::new();
    headers.insert(
        "x-ratelimit-reset",
        HeaderValue::from_str(&in_a_minute).unwrap(),
    );
    let wait = retry_delay(&headers, "https://example.com/x", 5.0);
    assert!(
        wait.as_secs_f64() > 0.0 && wait.as_secs_f64() <= 61.0,
        "the wait tracks the reset header: {wait:?}"
    );

    // a reset in the past means no wait at all, never a negative one
    let in_the_past = timeutil::fmt_state_ts(timeutil::now() - time::Duration::hours(1));
    let mut headers = HeaderMap::new();
    headers.insert(
        "x-ratelimit-reset",
        HeaderValue::from_str(&in_the_past).unwrap(),
    );
    assert_eq!(
        retry_delay(&headers, "https://example.com/x", 5.0),
        Duration::from_secs(0)
    );
}

#[test]
fn callback_url_decodes_the_query_it_read_and_keeps_valueless_pairs() {
    // '+' decodes to a space, %XX decodes to its byte, a pair without
    // '=' keeps its blank value, and a URL with no query gains one
    assert_eq!(
        build_callback_url(
            "https://watch.example/ping?a+b=c%2Fd&flag",
            &[("z".to_string(), "1".to_string())]
        ),
        "https://watch.example/ping?a+b=c%2Fd&flag=&z=1"
    );
    assert_eq!(
        build_callback_url("https://watch.example/ping", &[]),
        "https://watch.example/ping"
    );
}

#[test]
fn decode_component_speaks_unquote_plus() {
    assert_eq!(decode_component("a+b"), "a b");
    assert_eq!(decode_component("%2Fx"), "/x");
    assert_eq!(decode_component("plain"), "plain");
    // a truncated escape stays literal
    assert_eq!(decode_component("a%2"), "a%2");
}

#[tokio::test]
async fn a_stream_request_to_a_dead_port_is_an_error() {
    let dir = std::env::temp_dir().join(format!("ff-http-test-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let config = crate::config::Config {
        state_dir: dir,
        ..Default::default()
    };
    let db = crate::db::open(&config).await.unwrap();
    let http = HttpClient::new(config, db);
    // port 1 on loopback: nothing listens there
    let err = http
        .get_stream("http://127.0.0.1:1/stream")
        .await
        .unwrap_err();
    assert!(err.to_string().contains("127.0.0.1:1"), "{err}");
}

#[tokio::test]
async fn a_disabled_robots_cache_fetches_and_discards_nothing() {
    let dir = std::env::temp_dir().join(format!("ff-http-test-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let config = crate::config::Config {
        state_dir: dir,
        ..Default::default()
    };
    let db = crate::db::open(&config).await.unwrap();
    let cache = RobotsCache::new(db.clone(), Vec::new(), false, HttpClient::new(config, db));
    assert!(
        cache
            .can_fetch("agent", "https://example.com/x")
            .await
            .unwrap()
    );
    assert_eq!(cache.discard_stale(time::Duration::hours(1)).await, 0);
}
