//! The HTTP client against a real server: robots.txt, the blocklist, and
//! rate limits. Mirrors the Python original's test_http.py, with httpmock
//! standing in for the mocked `requests` session.

mod capture;
mod common;
mod fixture;

use std::sync::atomic::{AtomicUsize, Ordering};

use fedifetcher::http::HttpClient;
use fedifetcher::{VERSION, db, timeutil};
use httpmock::prelude::*;
use httpmock::{Method, Mock};

const ALLOW_ALL: &str = "User-agent: *\nAllow: /\n";
const DENY_ALL: &str = "User-agent: *\nDisallow: /\n";

/// Serve an allow-everything robots.txt, so a test can get on with the part
/// it is actually about.
async fn allow_all_robots(server: &MockServer) -> Mock<'_> {
    server
        .mock_async(|when, then| {
            when.method(Method::GET).path("/robots.txt");
            then.status(200).body(ALLOW_ALL);
        })
        .await
}

#[tokio::test]
async fn user_agent_names_the_configured_server() {
    let dir = common::tmp_dir();
    let config = common::test_config(&dir, "example.social");
    let db = fixture::test_db(&config).await;
    let client = HttpClient::new(config, db);
    assert_eq!(
        client.user_agent(),
        format!("FediFetcher/{VERSION}; +example.social (https://go.thms.uk/ff)")
    );
}

#[tokio::test]
async fn user_agent_can_hide_the_server() {
    let dir = common::tmp_dir();
    let mut config = common::test_config(&dir, "example.social");
    config.hide_server_in_user_agent = true;
    let db = fixture::test_db(&config).await;
    let client = HttpClient::new(config, db);
    assert_eq!(
        client.user_agent(),
        format!("FediFetcher/{VERSION} (https://go.thms.uk/ff)")
    );
}

#[tokio::test]
async fn get_sends_our_user_agent() {
    let server = MockServer::start_async().await;
    let dir = common::tmp_dir();
    let config = common::test_config(&dir, "example.social");
    let db = fixture::test_db(&config).await;
    let client = HttpClient::new(config, db);
    let api = server
        .mock_async(|when, then| {
            when.method(Method::GET)
                .path("/api")
                .header("User-Agent", client.user_agent());
            then.status(200).body("{}");
        })
        .await;
    allow_all_robots(&server).await;

    client
        .get(&format!("{}/api", server.base_url()), None)
        .await
        .expect("get should succeed");
    assert_eq!(api.calls(), 1);
}

#[tokio::test]
async fn a_failed_request_names_the_cause() {
    let dir = common::tmp_dir();
    let mut config = common::test_config(&dir, "example.social");
    config.respect_robots_txt = false;
    let db = fixture::test_db(&config).await;
    let client = HttpClient::new(config, db);

    // port 1 on localhost refuses connections: no server and no network
    // needed to produce a transport error
    let message = match client.get("http://127.0.0.1:1/api", None).await {
        Ok(_) => panic!("a refused connection is an error"),
        Err(err) => err.to_string(),
    };
    assert!(
        message
            .starts_with("Error requesting http://127.0.0.1:1/api: error sending request for url"),
        "{message}"
    );
    assert!(
        message.contains("client error (Connect)"),
        "the message carries the cause chain, not just reqwest's summary: {message}"
    );
}

#[tokio::test]
async fn requests_the_robots_file_forbids_are_refused() {
    let server = MockServer::start_async().await;
    let dir = common::tmp_dir();
    let robots = server
        .mock_async(|when, then| {
            when.method(Method::GET).path("/robots.txt");
            then.status(200).body(DENY_ALL);
        })
        .await;
    let api = server
        .mock_async(|when, then| {
            when.method(Method::GET).path("/api");
            then.status(200).body("{}");
        })
        .await;
    let config = common::test_config(&dir, "example.social");
    let db = fixture::test_db(&config).await;
    let client = HttpClient::new(config, db);

    let err = client
        .get(&format!("{}/api", server.base_url()), None)
        .await
        .err()
        .expect("a robots.txt that forbids everything must refuse the request");
    assert!(
        err.to_string().contains("prohibited by robots.txt"),
        "{err}"
    );
    assert_eq!(robots.calls(), 1);
    assert_eq!(api.calls(), 0);
}

#[tokio::test]
async fn robots_is_not_consulted_when_disabled_in_the_config() {
    let server = MockServer::start_async().await;
    let dir = common::tmp_dir();
    let robots = server
        .mock_async(|when, then| {
            when.method(Method::GET).path("/robots.txt");
            then.status(200).body(DENY_ALL);
        })
        .await;
    let api = server
        .mock_async(|when, then| {
            when.method(Method::GET).path("/api");
            then.status(200).body("{}");
        })
        .await;
    let mut config = common::test_config(&dir, "example.social");
    config.respect_robots_txt = false;
    let db = fixture::test_db(&config).await;
    let client = HttpClient::new(config, db);

    let resp = client
        .get(&format!("{}/api", server.base_url()), None)
        .await
        .expect("with robots.txt support off, the request should go ahead");
    assert_eq!(resp.status, 200);
    assert_eq!(robots.calls(), 0);
    assert_eq!(api.calls(), 1);
}

#[tokio::test]
async fn a_client_ignoring_robots_is_not_checked() {
    let server = MockServer::start_async().await;
    let dir = common::tmp_dir();
    let robots = server
        .mock_async(|when, then| {
            when.method(Method::GET).path("/robots.txt");
            then.status(200).body(DENY_ALL);
        })
        .await;
    let api = server
        .mock_async(|when, then| {
            when.method(Method::GET).path("/api");
            then.status(200).body("{}");
        })
        .await;
    let config = common::test_config(&dir, "example.social");
    let db = fixture::test_db(&config).await;
    let client = HttpClient::new(config, db).ignoring_robots();

    let resp = client
        .get(&format!("{}/api", server.base_url()), None)
        .await
        .expect("a client ignoring robots.txt should not be checked");
    assert_eq!(resp.status, 200);
    assert_eq!(robots.calls(), 0);
    assert_eq!(api.calls(), 1);
}

#[tokio::test]
async fn blocklisted_hosts_are_never_contacted() {
    let server = MockServer::start_async().await;
    let dir = common::tmp_dir();
    let api = server
        .mock_async(|when, then| {
            when.method(Method::GET).path("/api");
            then.status(200).body("{}");
        })
        .await;
    let netloc = server.base_url().trim_start_matches("http://").to_string();
    let mut config = common::test_config(&dir, "example.social");
    config.instance_blocklist = vec![netloc];
    let db = fixture::test_db(&config).await;
    let client = HttpClient::new(config, db);

    let err = client
        .get(&format!("{}/api", server.base_url()), None)
        .await
        .err()
        .expect("a blocklisted host must be refused");
    assert!(err.to_string().contains("blocklist"), "{err}");
    assert_eq!(api.calls(), 0);
}

#[tokio::test]
async fn a_host_blocked_by_robots_txt_is_logged_as_such() {
    let server = MockServer::start_async().await;
    let dir = common::tmp_dir();
    let robots = server
        .mock_async(|when, then| {
            when.method(Method::GET).path("/robots.txt");
            then.status(200).body(DENY_ALL);
        })
        .await;
    let api = server
        .mock_async(|when, then| {
            when.method(Method::GET).path("/api");
            then.status(200).body("{}");
        })
        .await;
    let config = common::test_config(&dir, "example.social");
    let db = fixture::test_db(&config).await;
    let client = HttpClient::new(config, db);

    let logs = capture::LogCapture::new();
    let guard = logs.install();
    let err = client
        .get(&format!("{}/api", server.base_url()), None)
        .await
        .err()
        .expect("a robots.txt that forbids everything must refuse the request");
    drop(guard);

    assert!(
        err.to_string().contains("prohibited by robots.txt"),
        "{err}"
    );
    assert_eq!(api.calls(), 0);
    assert_eq!(robots.calls(), 1);
    let logged = logs.text();
    let line = logged
        .lines()
        .find(|line| line.contains("Not fetching"))
        .unwrap_or_else(|| panic!("the skip should be logged, got:\n{logged}"));
    assert!(
        line.contains(&format!("{}/api", server.base_url())),
        "the log line names the URL: {line}"
    );
    assert!(line.contains("robots.txt disallows it"), "{line}");
    println!("logged: {line}");
}

#[tokio::test]
async fn a_blocklisted_host_is_logged_as_such() {
    let server = MockServer::start_async().await;
    let dir = common::tmp_dir();
    let api = server
        .mock_async(|when, then| {
            when.method(Method::GET).path("/api");
            then.status(200).body("{}");
        })
        .await;
    let netloc = server.base_url().trim_start_matches("http://").to_string();
    let mut config = common::test_config(&dir, "example.social");
    config.instance_blocklist = vec![netloc.clone()];
    let db = fixture::test_db(&config).await;
    let client = HttpClient::new(config, db);

    let logs = capture::LogCapture::new();
    let guard = logs.install();
    let err = client
        .get(&format!("{}/api", server.base_url()), None)
        .await
        .err()
        .expect("a blocklisted host must be refused");
    drop(guard);

    assert!(err.to_string().contains("blocklist"), "{err}");
    assert_eq!(api.calls(), 0);
    let logged = logs.text();
    let line = logged
        .lines()
        .find(|line| line.contains("Not fetching"))
        .unwrap_or_else(|| panic!("the skip should be logged, got:\n{logged}"));
    assert!(
        line.contains(&netloc),
        "the log line names the instance: {line}"
    );
    assert!(line.contains("blocklist"), "{line}");
    println!("logged: {line}");
}

/// Counts how often the rate-limited answer has been served, so one mock can
/// answer 429 a fixed number of times before another mock takes over. (The
/// matcher is a plain function, so it reaches a static.)
static RATE_LIMITED_HITS: AtomicUsize = AtomicUsize::new(0);

#[tokio::test]
async fn rate_limited_requests_are_retried_after_the_reset() {
    let server = MockServer::start_async().await;
    let dir = common::tmp_dir();
    let reset = (timeutil::now() + time::Duration::seconds(1))
        .unix_timestamp()
        .to_string();
    let limited = server
        .mock_async(|when, then| {
            when.method(Method::GET)
                .path("/api")
                .is_true(|_| RATE_LIMITED_HITS.fetch_add(1, Ordering::SeqCst) == 0);
            then.status(429).header("x-ratelimit-reset", reset);
        })
        .await;
    let ok = server
        .mock_async(|when, then| {
            when.method(Method::GET)
                .path("/api")
                .is_true(|_| RATE_LIMITED_HITS.load(Ordering::SeqCst) >= 1);
            then.status(200).body("{}");
        })
        .await;
    let config = common::test_config(&dir, "example.social");
    let db = fixture::test_db(&config).await;
    let client = HttpClient::new(config, db);

    let resp = client
        .get(&format!("{}/api", server.base_url()), None)
        .await
        .expect("the retry after the reset should succeed");
    assert_eq!(resp.status, 200);
    assert_eq!(limited.calls(), 1);
    assert_eq!(ok.calls(), 1);
}

static BACKOFF_HITS: AtomicUsize = AtomicUsize::new(0);

#[tokio::test]
async fn rate_limited_requests_back_off_without_a_reset_header() {
    let server = MockServer::start_async().await;
    let dir = common::tmp_dir();
    let limited = server
        .mock_async(|when, then| {
            when.method(Method::GET)
                .path("/api")
                .is_true(|_| BACKOFF_HITS.fetch_add(1, Ordering::SeqCst) < 2);
            then.status(429);
        })
        .await;
    let ok = server
        .mock_async(|when, then| {
            when.method(Method::GET)
                .path("/api")
                .is_true(|_| BACKOFF_HITS.load(Ordering::SeqCst) >= 2);
            then.status(200).body("{}");
        })
        .await;
    let mut config = common::test_config(&dir, "example.social");
    // two steps, so the third try succeeds: the series is walked in order
    config.rate_limit_cooldown = vec![0.01, 0.04];
    let db = fixture::test_db(&config).await;
    let client = HttpClient::new(config, db);

    let resp = client
        .get(&format!("{}/api", server.base_url()), None)
        .await
        .expect("the third try should succeed");
    assert_eq!(resp.status, 200);
    assert_eq!(limited.calls(), 2);
    assert_eq!(ok.calls(), 1);
}

#[tokio::test]
async fn rate_limiting_gives_up_eventually() {
    let server = MockServer::start_async().await;
    let dir = common::tmp_dir();
    let limited = server
        .mock_async(|when, then| {
            when.method(Method::GET).path("/api");
            then.status(429);
        })
        .await;
    let mut config = common::test_config(&dir, "example.social");
    // two steps: the initial request, plus one per step, then it gives up
    config.rate_limit_cooldown = vec![0.01, 0.01];
    let db = fixture::test_db(&config).await;
    let client = HttpClient::new(config, db);

    let err = client
        .get(&format!("{}/api", server.base_url()), None)
        .await
        .err()
        .expect("a request that never stops being rate limited must fail");
    assert!(
        err.to_string().contains("Maximum number of retries"),
        "{err}"
    );
    // the initial request, plus one per retry
    assert_eq!(limited.calls(), 3);
}

#[tokio::test]
async fn robots_is_read_from_the_network_once_then_remembered() {
    let server = MockServer::start_async().await;
    let dir = common::tmp_dir();
    let robots = allow_all_robots(&server).await;
    let api = server
        .mock_async(|when, then| {
            when.method(Method::GET).path("/api");
            then.status(200).body("{}");
        })
        .await;
    let config = common::test_config(&dir, "example.social");
    let db = fixture::test_db(&config).await;
    let client = HttpClient::new(config, db);
    let url = format!("{}/api", server.base_url());

    client.get(&url, None).await.expect("first get");
    client.get(&url, None).await.expect("second get");

    assert_eq!(robots.calls(), 1, "robots.txt should be fetched only once");
    assert_eq!(api.calls(), 2);
}

#[tokio::test]
async fn robots_is_cached_in_the_database_between_clients() {
    let server = MockServer::start_async().await;
    let dir = common::tmp_dir();
    let robots = allow_all_robots(&server).await;
    let api = server
        .mock_async(|when, then| {
            when.method(Method::GET).path("/api");
            then.status(200).body("{}");
        })
        .await;
    let url = format!("{}/api", server.base_url());
    let robots_url = format!("{}/robots.txt", server.base_url());
    let config = common::test_config(&dir, "example.social");
    let db = fixture::test_db(&config).await;

    let first = HttpClient::new(config.clone(), db.clone());
    first.get(&url, None).await.expect("first client");
    assert_eq!(robots.calls(), 1);

    let second = HttpClient::new(config, db.clone());
    second.get(&url, None).await.expect("second client");
    assert_eq!(
        robots.calls(),
        1,
        "the second client should read the cached robots.txt"
    );
    assert_eq!(api.calls(), 2);

    assert_eq!(
        db::robots_cached(&db, &robots_url).await.unwrap(),
        Some(ALLOW_ALL.to_string()),
        "the cache row holds the fetched text"
    );
}

#[tokio::test]
async fn a_refused_robots_file_denies_everything() {
    let server = MockServer::start_async().await;
    let dir = common::tmp_dir();
    let robots = server
        .mock_async(|when, then| {
            when.method(Method::GET).path("/robots.txt");
            then.status(403);
        })
        .await;
    let api = server
        .mock_async(|when, then| {
            when.method(Method::GET).path("/api");
            then.status(200).body("{}");
        })
        .await;
    let config = common::test_config(&dir, "example.social");
    let db = fixture::test_db(&config).await;
    let client = HttpClient::new(config, db);

    let err = client
        .get(&format!("{}/api", server.base_url()), None)
        .await
        .err()
        .expect("a refused robots.txt must deny everything");
    assert!(
        err.to_string().contains("prohibited by robots.txt"),
        "{err}"
    );
    assert_eq!(robots.calls(), 1);
    assert_eq!(api.calls(), 0);
}

#[tokio::test]
async fn an_unreachable_robots_file_allows_everything() {
    let dir = common::tmp_dir();
    let config = common::test_config(&dir, "example.social");
    let db = fixture::test_db(&config).await;
    let client = HttpClient::new(config, db);

    // port 9 (discard) is closed: nothing answers there
    let allowed = client
        .robots()
        .can_fetch("FediFetcher/test", "https://127.0.0.1:9/api")
        .await
        .expect("an unreachable host should not be an error");
    assert!(allowed, "an unreachable robots.txt should allow everything");
}

#[tokio::test]
async fn stale_robots_entries_are_discarded() {
    let dir = common::tmp_dir();
    let config = common::test_config(&dir, "example.social");
    let db = fixture::test_db(&config).await;
    let robots = HttpClient::new(config, db.clone()).robots();

    db::robots_store(&db, "https://old.example/robots.txt", ALLOW_ALL)
        .await
        .unwrap();
    db::robots_store(&db, "https://new.example/robots.txt", ALLOW_ALL)
        .await
        .unwrap();

    // age one entry beyond the cache's lifetime
    let two_days_ago = timeutil::fmt_state_ts(timeutil::now() - time::Duration::days(2));
    sqlx::query("UPDATE robots_cache SET fetched_at = ? WHERE url = ?")
        .bind(&two_days_ago)
        .bind("https://old.example/robots.txt")
        .execute(&db)
        .await
        .unwrap();

    assert_eq!(robots.discard_stale(time::Duration::days(1)).await, 1);
    assert_eq!(
        db::robots_cached(&db, "https://old.example/robots.txt")
            .await
            .unwrap(),
        None
    );
    assert_eq!(
        db::robots_cached(&db, "https://new.example/robots.txt")
            .await
            .unwrap(),
        Some(ALLOW_ALL.to_string())
    );
}

#[tokio::test]
async fn get_redirect_url_returns_the_url_when_it_does_not_redirect() {
    let server = MockServer::start_async().await;
    let dir = common::tmp_dir();
    allow_all_robots(&server).await;
    let object = server
        .mock_async(|when, then| {
            when.method(Method::HEAD).path("/objects/1");
            then.status(200);
        })
        .await;
    let config = common::test_config(&dir, "example.social");
    let db = fixture::test_db(&config).await;
    let client = HttpClient::new(config, db);
    let url = format!("{}/objects/1", server.base_url());

    assert_eq!(
        client.get_redirect_url(&url).await.as_deref(),
        Some(url.as_str())
    );
    assert_eq!(object.calls(), 1);
}

#[tokio::test]
async fn get_redirect_url_follows_a_302() {
    let server = MockServer::start_async().await;
    let dir = common::tmp_dir();
    allow_all_robots(&server).await;
    let object = server
        .mock_async(|when, then| {
            when.method(Method::HEAD).path("/objects/1");
            then.status(302).header("Location", "/notice/123");
        })
        .await;
    let config = common::test_config(&dir, "example.social");
    let db = fixture::test_db(&config).await;
    let client = HttpClient::new(config, db);

    assert_eq!(
        client
            .get_redirect_url(&format!("{}/objects/1", server.base_url()))
            .await
            .as_deref(),
        Some("/notice/123")
    );
    assert_eq!(object.calls(), 1);
}

#[tokio::test]
async fn get_redirect_url_gives_up_on_other_statuses() {
    let server = MockServer::start_async().await;
    let dir = common::tmp_dir();
    allow_all_robots(&server).await;
    server
        .mock_async(|when, then| {
            when.method(Method::HEAD).path("/objects/1");
            then.status(500);
        })
        .await;
    let config = common::test_config(&dir, "example.social");
    let db = fixture::test_db(&config).await;
    let client = HttpClient::new(config, db);

    assert_eq!(
        client
            .get_redirect_url(&format!("{}/objects/1", server.base_url()))
            .await,
        None
    );
}

#[tokio::test]
async fn get_redirect_url_honors_the_blocklist() {
    let server = MockServer::start_async().await;
    let dir = common::tmp_dir();
    let object = server
        .mock_async(|when, then| {
            when.method(Method::HEAD).path("/objects/1");
            then.status(200);
        })
        .await;
    let netloc = server.base_url().trim_start_matches("http://").to_string();
    let mut config = common::test_config(&dir, "example.social");
    config.instance_blocklist = vec![netloc];
    let db = fixture::test_db(&config).await;
    let client = HttpClient::new(config, db);

    assert_eq!(
        client
            .get_redirect_url(&format!("{}/objects/1", server.base_url()))
            .await,
        None
    );
    assert_eq!(object.calls(), 0);
}

#[tokio::test]
async fn get_redirect_url_honors_robots() {
    let server = MockServer::start_async().await;
    let dir = common::tmp_dir();
    let robots = server
        .mock_async(|when, then| {
            when.method(Method::GET).path("/robots.txt");
            then.status(200).body(DENY_ALL);
        })
        .await;
    let object = server
        .mock_async(|when, then| {
            when.method(Method::HEAD).path("/objects/1");
            then.status(200);
        })
        .await;
    let config = common::test_config(&dir, "example.social");
    let db = fixture::test_db(&config).await;
    let client = HttpClient::new(config, db);

    assert_eq!(
        client
            .get_redirect_url(&format!("{}/objects/1", server.base_url()))
            .await,
        None
    );
    assert_eq!(robots.calls(), 1);
    assert_eq!(object.calls(), 0);
}

#[tokio::test]
async fn authenticated_clients_send_their_token_and_skip_robots() {
    let server = MockServer::start_async().await;
    let dir = common::tmp_dir();
    let robots = server
        .mock_async(|when, then| {
            when.method(Method::GET).path("/robots.txt");
            then.status(200).body(DENY_ALL);
        })
        .await;
    let api = server
        .mock_async(|when, then| {
            when.method(Method::GET)
                .path("/api")
                .header("Authorization", "Bearer secret");
            then.status(200).body("{}");
        })
        .await;
    let config = common::test_config(&dir, "example.social");
    let db = fixture::test_db(&config).await;
    let client = HttpClient::new(config, db);

    let resp = client
        .authenticated("secret")
        .get(&format!("{}/api", server.base_url()), None)
        .await
        .expect("an authenticated client does not get checked against robots.txt");
    assert_eq!(resp.status, 200);
    assert_eq!(api.calls(), 1);
    assert_eq!(robots.calls(), 0, "robots.txt is never asked about");
}

#[tokio::test]
async fn the_original_client_is_unaffected_by_deriving_one() {
    let server = MockServer::start_async().await;
    let dir = common::tmp_dir();
    allow_all_robots(&server).await;
    let authed = server
        .mock_async(|when, then| {
            when.method(Method::GET)
                .path("/authed")
                .header("Authorization", "Bearer secret");
            then.status(200).body("{}");
        })
        .await;
    let plain = server
        .mock_async(|when, then| {
            when.method(Method::GET).path("/plain").is_true(|req| {
                req.headers_vec()
                    .iter()
                    .all(|(name, _)| !name.eq_ignore_ascii_case("authorization"))
            });
            then.status(200).body("{}");
        })
        .await;
    let config = common::test_config(&dir, "example.social");
    let db = fixture::test_db(&config).await;
    let client = HttpClient::new(config, db);

    client
        .authenticated("secret")
        .get(&format!("{}/authed", server.base_url()), None)
        .await
        .expect("authenticated get");
    client
        .get(&format!("{}/plain", server.base_url()), None)
        .await
        .expect("plain get");

    assert_eq!(authed.calls(), 1);
    assert_eq!(plain.calls(), 1, "the original client sends no token");
}

#[tokio::test]
async fn post_sends_the_body_as_json_and_the_client_headers() {
    let server = MockServer::start_async().await;
    let dir = common::tmp_dir();
    allow_all_robots(&server).await;
    let api = server
        .mock_async(|when, then| {
            when.method(Method::POST)
                .path("/api")
                .header("Authorization", "Bearer secret")
                .json_body(serde_json::json!({"key": "value"}));
            then.status(200).body("{}");
        })
        .await;
    let config = common::test_config(&dir, "example.social");
    let db = fixture::test_db(&config).await;
    let client = HttpClient::new(config, db);

    let resp = client
        .authenticated("secret")
        .post(
            &format!("{}/api", server.base_url()),
            &serde_json::json!({"key": "value"}),
            None,
        )
        .await
        .expect("post should succeed");
    assert_eq!(resp.status, 200);
    assert_eq!(api.calls(), 1);
}
