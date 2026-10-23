//! Manual fetches: the posts a run records as trigger posts, and the
//! at-will fetches the web UI's buttons start — one post's context, one
//! account's posts, one host's server info, one URL's resolve.

mod common;

use fedifetcher::api::mastodon::HomeServer;
use fedifetcher::backfill::UserSet;
use fedifetcher::context::known_context_urls;
use fedifetcher::db::{self, TriggerPost};
use fedifetcher::fetch;
use fedifetcher::http::HttpClient;
use fedifetcher::posts::Post;
use fedifetcher::servers::ServerInfo;
use fedifetcher::store::State;
use fedifetcher::timeutil;
use fedifetcher::users::User;
use httpmock::prelude::*;
use httpmock::Method;

/// The mock server's host, without the scheme.
fn host(server: &MockServer) -> String {
    server.base_url().trim_start_matches("http://").to_string()
}

/// A post on a server that is not there: `http://` URLs have no host as
/// `host_of` reads them, so resolving them fails before any request.
const UNREACHABLE: &str = "http://127.0.0.1:9";

fn post(uri: &str, is_public: bool) -> Post {
    Post {
        url: uri.to_string(),
        uri: uri.to_string(),
        created_at: timeutil::now(),
        is_public,
        reblog: None,
        in_reply_to_id: None,
        in_reply_to_account_id: None,
        reply_count: Some(4),
        account: Some(User::new(
            "alice@remote.example",
            "https://remote.example/@alice",
        )),
        mentions: vec![],
    }
}

/// A context scan records every post it was given as a trigger post, with
/// its author, its reply count, and the source that scanned it.
#[tokio::test]
async fn a_scan_records_its_trigger_posts() {
    let dir = common::tmp_dir();
    let config = common::test_config(&dir, "home.example");
    let db = db::open(&config).await.unwrap();
    let http = HttpClient::new(config.clone(), db.clone());
    let home = HomeServer::new("home.example", "token", http.clone());
    let mut state = State::default();
    let mut ctx = common::run_context(&config, &mut state, &http, &home, &db);

    let private = post(&format!("{UNREACHABLE}/@alice/10"), false);
    let public = post(&format!("{UNREACHABLE}/@alice/11"), true);
    let urls = known_context_urls(
        &mut ctx,
        &[private.clone(), public.clone()],
        "home timeline",
        false,
    )
    .await;

    assert!(
        urls.is_empty(),
        "no context: neither post's server is reachable"
    );
    for toot in [&private, &public] {
        let recorded = db::trigger_post(&db, &toot.uri)
            .await
            .unwrap()
            .expect("the scanned post is recorded");
        assert_eq!(recorded.source, "home timeline");
        assert_eq!(recorded.acct.as_deref(), Some("alice@remote.example"));
        assert_eq!(recorded.replies_count, Some(4));
        assert_eq!(recorded.url, toot.url);
        assert!(recorded.created_at.is_some());
    }
    assert!(db::is_private_post(&db, &private.uri).await.unwrap());
    assert!(!db::is_private_post(&db, &public.uri).await.unwrap());
}

/// A manual post fetch resolves the post on our server, and records it as a
/// trigger post, even when its own server cannot be reached for context.
#[tokio::test]
async fn force_post_context_resolves_the_post_and_records_it() {
    let server = MockServer::start_async().await;
    let dir = common::tmp_dir();
    let config = common::test_config(&dir, "home.example");
    let db = db::open(&config).await.unwrap();
    let http = HttpClient::new(config.clone(), db.clone());
    let home = HomeServer::with_scheme(&host(&server), "http", "secret-token", http.clone());
    let mut state = State::default();
    let mut ctx = common::run_context(&config, &mut state, &http, &home, &db);

    let uri = format!("{UNREACHABLE}/@alice/10");
    let search = server
        .mock_async(|when, then| {
            when.method(Method::GET)
                .path("/api/v2/search")
                .query_param("q", uri.as_str())
                .query_param("resolve", "true");
            then.status(200);
        })
        .await;

    fetch::force_post_context(&mut ctx, &uri).await;

    assert_eq!(
        search.hits(),
        1,
        "the post itself is resolved on our server"
    );
    assert!(
        state.seen_urls.contains(&uri),
        "the resolved post is remembered"
    );
    let recorded = db::trigger_post(&db, &uri)
        .await
        .unwrap()
        .expect("the fetched post is recorded");
    assert_eq!(recorded.source, "manual");
}

/// A post recorded as private is not fetched manually: not even resolved.
#[tokio::test]
async fn force_post_context_skips_a_post_recorded_as_private() {
    let server = MockServer::start_async().await;
    let dir = common::tmp_dir();
    let config = common::test_config(&dir, "home.example");
    let db = db::open(&config).await.unwrap();
    let http = HttpClient::new(config.clone(), db.clone());
    let home = HomeServer::with_scheme(&host(&server), "http", "secret-token", http.clone());
    let mut state = State::default();
    let mut ctx = common::run_context(&config, &mut state, &http, &home, &db);

    let uri = format!("{UNREACHABLE}/@alice/10");
    db::mark_private_post(&db, &uri).await.unwrap();
    let search = server
        .mock_async(|when, then| {
            when.method(Method::GET).path("/api/v2/search");
            then.status(200);
        })
        .await;

    let logs = common::LogCapture::new();
    let guard = logs.install();
    fetch::force_post_context(&mut ctx, &uri).await;
    drop(guard);

    assert_eq!(search.hits(), 0, "a private post is not even resolved");
    assert!(
        logs.text().contains("recorded as private"),
        "{}",
        logs.text()
    );
    assert!(state.seen_urls.is_empty());
}

/// An account recorded as opted out is not backfilled manually.
#[tokio::test]
async fn force_user_posts_refuses_an_opted_out_account() {
    let server = MockServer::start_async().await;
    let dir = common::tmp_dir();
    let config = common::test_config(&dir, "home.example");
    let db = db::open(&config).await.unwrap();
    let http = HttpClient::new(config.clone(), db.clone());
    let home = HomeServer::with_scheme(&host(&server), "http", "secret-token", http.clone());
    let mut state = State::default();
    let mut ctx = common::run_context(&config, &mut state, &http, &home, &db);

    // the first things a fetch would ask for, if the refusal did not happen
    let nodeinfo = server
        .mock_async(|when, then| {
            when.method(Method::GET).path("/.well-known/nodeinfo");
            then.status(200).body("{}");
        })
        .await;
    let robots = server
        .mock_async(|when, then| {
            when.method(Method::GET).path("/robots.txt");
            then.status(200).body("User-agent: *\nAllow: /\n");
        })
        .await;

    db::mark_opted_out_account(&db, "bot@remote.example", "it is not indexable")
        .await
        .unwrap();

    let logs = common::LogCapture::new();
    let guard = logs.install();
    fetch::force_user_posts(
        &mut ctx,
        "bot@remote.example",
        UserSet::RecentlyCheckedUsers,
    )
    .await;
    drop(guard);

    assert_eq!(
        nodeinfo.hits(),
        0,
        "nothing is fetched for an opted-out account"
    );
    assert_eq!(
        robots.hits(),
        0,
        "nothing is fetched for an opted-out account"
    );
    assert!(logs.text().contains("opted out"), "{}", logs.text());
    assert!(logs.text().contains("bot@remote.example"));
}

/// A local account has nothing to backfill.
#[tokio::test]
async fn force_user_posts_refuses_a_local_account() {
    let dir = common::tmp_dir();
    let config = common::test_config(&dir, "home.example");
    let db = db::open(&config).await.unwrap();
    let http = HttpClient::new(config.clone(), db.clone());
    let home = HomeServer::with_scheme("home.example", "http", "secret-token", http.clone());
    let mut state = State::default();
    let mut ctx = common::run_context(&config, &mut state, &http, &home, &db);

    let logs = common::LogCapture::new();
    let guard = logs.install();
    fetch::force_user_posts(&mut ctx, "alice", UserSet::KnownFollowings).await;
    drop(guard);

    assert!(logs.text().contains("local user"), "{}", logs.text());
    assert!(
        state.known_followings.is_empty(),
        "the account is not recorded as checked"
    );
}

/// Re-resolving a URL remembers it when our server resolves it, and does
/// not when it does not.
#[tokio::test]
async fn force_resolve_url_adds_the_url_to_seen_urls() {
    let server = MockServer::start_async().await;
    let dir = common::tmp_dir();
    let config = common::test_config(&dir, "home.example");
    let db = db::open(&config).await.unwrap();
    let http = HttpClient::new(config, db.clone());
    let home = HomeServer::with_scheme(&host(&server), "http", "secret-token", http);
    let mut state = State::default();

    let url = "https://remote.example/@alice/1";
    let search = server
        .mock_async(|when, then| {
            when.method(Method::GET)
                .path("/api/v2/search")
                .query_param("q", url)
                .query_param("resolve", "true");
            then.status(200);
        })
        .await;
    fetch::force_resolve_url(&home, &mut state, url).await;
    assert_eq!(search.hits(), 1);
    assert!(state.seen_urls.contains(url));

    let refused = "https://remote.example/@alice/2";
    server
        .mock_async(|when, then| {
            when.method(Method::GET)
                .path("/api/v2/search")
                .query_param("q", refused);
            then.status(500);
        })
        .await;
    fetch::force_resolve_url(&home, &mut state, refused).await;
    assert!(!state.seen_urls.contains(refused));
}

/// A manual host re-check ignores the cached entry: the lookup runs again,
/// and its failure is recorded in its place.
#[tokio::test]
async fn force_host_info_replaces_the_cached_entry() {
    let dir = common::tmp_dir();
    let config = common::test_config(&dir, "home.example");
    let db = db::open(&config).await.unwrap();
    let http = HttpClient::new(config, db.clone());
    let mut state = State::default();

    // a cached entry for a host that is not there
    let host = "127.0.0.1:9";
    let nodeinfo = serde_json::json!({
        "software": {"name": "mastodon", "version": "4.3.0"},
        "protocols": ["activitypub"],
    });
    state.seen_hosts.add(
        host,
        ServerInfo::from_nodeinfo(host, &nodeinfo)
            .unwrap()
            .to_state(),
    );
    assert!(state.seen_hosts.contains(host));

    let logs = common::LogCapture::new();
    let guard = logs.install();
    fetch::force_host_info(host, &http, &mut state).await;
    drop(guard);

    // the cached copy is gone, replaced by the failed lookup's record
    let entry = state.seen_hosts.get(host).expect("the lookup is recorded");
    assert_eq!(entry.get("info"), Some(&serde_json::Value::Null));
    assert!(
        entry
            .get("error")
            .and_then(serde_json::Value::as_str)
            .is_some(),
        "the failure's reason is kept: {entry}"
    );
    assert!(
        logs.text().contains("could not re-check"),
        "{}",
        logs.text()
    );
}

/// The trigger-post record a scan writes round-trips through the database.
#[tokio::test]
async fn trigger_posts_round_trip_through_the_scan() {
    let dir = common::tmp_dir();
    let config = common::test_config(&dir, "home.example");
    let db = db::open(&config).await.unwrap();
    let http = HttpClient::new(config.clone(), db.clone());
    let home = HomeServer::new("home.example", "token", http.clone());
    let mut state = State::default();
    let mut ctx = common::run_context(&config, &mut state, &http, &home, &db);

    let toot = post(&format!("{UNREACHABLE}/@alice/10"), true);
    known_context_urls(&mut ctx, std::slice::from_ref(&toot), "bookmarks", false).await;

    let recorded: TriggerPost = db::trigger_post(&db, &toot.uri).await.unwrap().unwrap();
    assert_eq!(recorded.uri, toot.uri);
    assert_eq!(recorded.url, toot.url);
    assert_eq!(recorded.source, "bookmarks");
}
