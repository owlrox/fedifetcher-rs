//! The HTTP client against a real server: robots.txt, the blocklist, and
//! rate limits. Mirrors the Python original's test_http.py, with httpmock
//! standing in for the mocked `requests` session.

mod common;

use std::sync::atomic::{AtomicUsize, Ordering};

use fedifetcher::http::{HttpClient, xxh128_hex};
use fedifetcher::{timeutil, VERSION};
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

#[test]
fn user_agent_names_the_configured_server() {
    let dir = common::tmp_dir();
    let client = HttpClient::new(common::test_config(&dir, "example.social"));
    assert_eq!(
        client.user_agent(),
        format!("FediFetcher/{VERSION}; +example.social (https://go.thms.uk/ff)")
    );
}

#[tokio::test]
async fn get_sends_our_user_agent() {
    let server = MockServer::start_async().await;
    let dir = common::tmp_dir();
    let client = HttpClient::new(common::test_config(&dir, "example.social"));
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
        .get(&format!("{}/api", server.base_url()), None, 5, 0.5)
        .await
        .ok()
        .expect("get should succeed");
    assert_eq!(api.hits(), 1);
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
    let client = HttpClient::new(common::test_config(&dir, "example.social"));

    let err = client
        .get(&format!("{}/api", server.base_url()), None, 5, 0.5)
        .await
        .err()
        .expect("a robots.txt that forbids everything must refuse the request");
    assert!(err.to_string().contains("prohibited by robots.txt"), "{err}");
    assert_eq!(robots.hits(), 1);
    assert_eq!(api.hits(), 0);
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
    let client = HttpClient::new(config);

    let resp = client
        .get(&format!("{}/api", server.base_url()), None, 5, 0.5)
        .await
        .ok()
        .expect("with robots.txt support off, the request should go ahead");
    assert_eq!(resp.status, 200);
    assert_eq!(robots.hits(), 0);
    assert_eq!(api.hits(), 1);
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
    let client = HttpClient::new(common::test_config(&dir, "example.social")).ignoring_robots();

    let resp = client
        .get(&format!("{}/api", server.base_url()), None, 5, 0.5)
        .await
        .ok()
        .expect("a client ignoring robots.txt should not be checked");
    assert_eq!(resp.status, 200);
    assert_eq!(robots.hits(), 0);
    assert_eq!(api.hits(), 1);
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
    let client = HttpClient::new(config);

    let err = client
        .get(&format!("{}/api", server.base_url()), None, 5, 0.5)
        .await
        .err()
        .expect("a blocklisted host must be refused");
    assert!(err.to_string().contains("blocklist"), "{err}");
    assert_eq!(api.hits(), 0);
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
            when.method(Method::GET).path("/api").matches(|_| {
                RATE_LIMITED_HITS.fetch_add(1, Ordering::SeqCst) == 0
            });
            then.status(429).header("x-ratelimit-reset", reset);
        })
        .await;
    let ok = server
        .mock_async(|when, then| {
            when.method(Method::GET)
                .path("/api")
                .matches(|_| RATE_LIMITED_HITS.load(Ordering::SeqCst) >= 1);
            then.status(200).body("{}");
        })
        .await;
    let client = HttpClient::new(common::test_config(&dir, "example.social"));

    let resp = client
        .get(&format!("{}/api", server.base_url()), None, 5, 0.5)
        .await
        .ok()
        .expect("the retry after the reset should succeed");
    assert_eq!(resp.status, 200);
    assert_eq!(limited.hits(), 1);
    assert_eq!(ok.hits(), 1);
}

static BACKOFF_HITS: AtomicUsize = AtomicUsize::new(0);

#[tokio::test]
async fn rate_limited_requests_back_off_without_a_reset_header() {
    let server = MockServer::start_async().await;
    let dir = common::tmp_dir();
    let limited = server
        .mock_async(|when, then| {
            when.method(Method::GET).path("/api").matches(|_| {
                BACKOFF_HITS.fetch_add(1, Ordering::SeqCst) < 2
            });
            then.status(429);
        })
        .await;
    let ok = server
        .mock_async(|when, then| {
            when.method(Method::GET)
                .path("/api")
                .matches(|_| BACKOFF_HITS.load(Ordering::SeqCst) >= 2);
            then.status(200).body("{}");
        })
        .await;
    let client = HttpClient::new(common::test_config(&dir, "example.social"));

    let resp = client
        .get(&format!("{}/api", server.base_url()), None, 5, 0.01)
        .await
        .ok()
        .expect("the third try should succeed");
    assert_eq!(resp.status, 200);
    assert_eq!(limited.hits(), 2);
    assert_eq!(ok.hits(), 1);
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
    let client = HttpClient::new(common::test_config(&dir, "example.social"));

    let err = client
        .get(&format!("{}/api", server.base_url()), None, 2, 0.01)
        .await
        .err()
        .expect("a request that never stops being rate limited must fail");
    assert!(
        err.to_string().contains("Maximum number of retries"),
        "{err}"
    );
    // the initial request, plus one per retry
    assert_eq!(limited.hits(), 3);
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
    let client = HttpClient::new(common::test_config(&dir, "example.social"));
    let url = format!("{}/api", server.base_url());

    client.get(&url, None, 5, 0.5).await.ok().expect("first get");
    client.get(&url, None, 5, 0.5).await.ok().expect("second get");

    assert_eq!(robots.hits(), 1, "robots.txt should be fetched only once");
    assert_eq!(api.hits(), 2);
}

#[tokio::test]
async fn robots_is_cached_on_disk_between_clients() {
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

    let first = HttpClient::new(common::test_config(&dir, "example.social"));
    first.get(&url, None, 5, 0.5).await.ok().expect("first client");
    assert_eq!(robots.hits(), 1);

    let second = HttpClient::new(common::test_config(&dir, "example.social"));
    second.get(&url, None, 5, 0.5).await.ok().expect("second client");
    assert_eq!(
        robots.hits(),
        1,
        "the second client should read the cached robots.txt"
    );
    assert_eq!(api.hits(), 2);

    // the cache file is named the way the Python original names it
    let cache_file = dir.join(format!("robots-{}.txt", xxh128_hex(&robots_url)));
    assert_eq!(
        std::fs::read_to_string(&cache_file).expect("cache file should exist"),
        ALLOW_ALL
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
    let client = HttpClient::new(common::test_config(&dir, "example.social"));

    let err = client
        .get(&format!("{}/api", server.base_url()), None, 5, 0.5)
        .await
        .err()
        .expect("a refused robots.txt must deny everything");
    assert!(err.to_string().contains("prohibited by robots.txt"), "{err}");
    assert_eq!(robots.hits(), 1);
    assert_eq!(api.hits(), 0);
}

#[tokio::test]
async fn an_unreachable_robots_file_allows_everything() {
    let dir = common::tmp_dir();
    let client = HttpClient::new(common::test_config(&dir, "example.social"));

    // port 9 (discard) is closed: nothing answers there
    let allowed = client
        .robots()
        .can_fetch("FediFetcher/test", "https://127.0.0.1:9/api")
        .await
        .ok()
        .expect("an unreachable host should not be an error");
    assert!(allowed, "an unreachable robots.txt should allow everything");
}

#[tokio::test]
async fn stale_robots_files_are_discarded() {
    let dir = common::tmp_dir();
    let client = HttpClient::new(common::test_config(&dir, "example.social"));
    let robots = client.robots();

    let stale = robots.cache_path("https://old.example/robots.txt");
    let fresh = robots.cache_path("https://new.example/robots.txt");
    std::fs::write(&stale, ALLOW_ALL).unwrap();
    std::fs::write(&fresh, ALLOW_ALL).unwrap();
    let unrelated = dir.join("seen_urls");
    std::fs::write(&unrelated, "").unwrap();

    let two_days_ago = std::time::SystemTime::now() - std::time::Duration::from_secs(2 * 24 * 3600);
    for path in [&stale, &unrelated] {
        let file = std::fs::OpenOptions::new()
            .write(true)
            .open(path)
            .unwrap();
        file.set_modified(two_days_ago).unwrap();
    }

    assert_eq!(robots.discard_stale_files(time::Duration::days(1)), 1);
    assert!(!stale.exists());
    assert!(fresh.exists());
    assert!(unrelated.exists(), "unrelated files are left alone");
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
    let client = HttpClient::new(common::test_config(&dir, "example.social"));
    let url = format!("{}/objects/1", server.base_url());

    assert_eq!(
        client.get_redirect_url(&url).await.as_deref(),
        Some(url.as_str())
    );
    assert_eq!(object.hits(), 1);
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
    let client = HttpClient::new(common::test_config(&dir, "example.social"));

    assert_eq!(
        client
            .get_redirect_url(&format!("{}/objects/1", server.base_url()))
            .await
            .as_deref(),
        Some("/notice/123")
    );
    assert_eq!(object.hits(), 1);
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
    let client = HttpClient::new(common::test_config(&dir, "example.social"));

    assert_eq!(
        client
            .get_redirect_url(&format!("{}/objects/1", server.base_url()))
            .await,
        None
    );
}

#[tokio::test]
async fn get_redirect_url_honours_the_blocklist() {
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
    let client = HttpClient::new(config);

    assert_eq!(
        client
            .get_redirect_url(&format!("{}/objects/1", server.base_url()))
            .await,
        None
    );
    assert_eq!(object.hits(), 0);
}

#[tokio::test]
async fn get_redirect_url_honours_robots() {
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
    let client = HttpClient::new(common::test_config(&dir, "example.social"));

    assert_eq!(
        client
            .get_redirect_url(&format!("{}/objects/1", server.base_url()))
            .await,
        None
    );
    assert_eq!(robots.hits(), 1);
    assert_eq!(object.hits(), 0);
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
    let client = HttpClient::new(common::test_config(&dir, "example.social"));

    let resp = client
        .authenticated("secret")
        .get(&format!("{}/api", server.base_url()), None, 5, 0.5)
        .await
        .ok()
        .expect("an authenticated client does not get checked against robots.txt");
    assert_eq!(resp.status, 200);
    assert_eq!(api.hits(), 1);
    assert_eq!(robots.hits(), 0, "robots.txt is never asked about");
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
            when.method(Method::GET).path("/plain").matches(|req| {
                req.headers
                    .as_ref()
                    .map(|headers| {
                        headers
                            .iter()
                            .all(|(name, _)| !name.eq_ignore_ascii_case("authorization"))
                    })
                    .unwrap_or(true)
            });
            then.status(200).body("{}");
        })
        .await;
    let client = HttpClient::new(common::test_config(&dir, "example.social"));

    client
        .authenticated("secret")
        .get(&format!("{}/authed", server.base_url()), None, 5, 0.5)
        .await
        .ok()
        .expect("authenticated get");
    client
        .get(&format!("{}/plain", server.base_url()), None, 5, 0.5)
        .await
        .ok()
        .expect("plain get");

    assert_eq!(authed.hits(), 1);
    assert_eq!(plain.hits(), 1, "the original client sends no token");
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
    let client = HttpClient::new(common::test_config(&dir, "example.social"));

    let resp = client
        .authenticated("secret")
        .post(
            &format!("{}/api", server.base_url()),
            &serde_json::json!({"key": "value"}),
            None,
            5,
            0.5,
        )
        .await
        .ok()
        .expect("post should succeed");
    assert_eq!(resp.status, 200);
    assert_eq!(api.hits(), 1);
}
