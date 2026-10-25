//! The context scan's outputs: which context URLs get added to the server,
//! and which replies to a reply are followed back to their own server.

mod capture;
mod common;
mod runner;

use fedifetcher::api::mastodon::HomeServer;
use fedifetcher::context::{
    add_context_urls, context_urls, known_context_urls, replied_toot_server_ids,
};
use fedifetcher::db;
use fedifetcher::http::HttpClient;
use fedifetcher::posts::Post;
use fedifetcher::store::{RepliedToot, State};
use fedifetcher::timeutil;
use fedifetcher::urls::PostRef;
use fedifetcher::users::User;
use httpmock::Method;
use httpmock::prelude::*;

/// The mock server's host, without the scheme.
fn host(server: &MockServer) -> String {
    server.base_url().trim_start_matches("http://").to_string()
}

/// A public reply from alice to bob's status `reply_to`.
fn reply_toot(reply_to: &str) -> Post {
    let url = format!("https://home.invalid/@alice/{reply_to}");
    Post {
        url: url.clone(),
        uri: url,
        created_at: timeutil::now(),
        is_public: true,
        reblog: None,
        in_reply_to_id: Some(reply_to.to_string()),
        in_reply_to_account_id: Some("7".to_string()),
        reply_count: Some(1),
        account: Some(User::new("alice", "https://home.invalid/@alice")),
        mentions: vec![User {
            acct: "bob".to_string(),
            url: "https://home.invalid/@bob".to_string(),
            id: Some("7".to_string()),
            ..User::new("bob", "https://home.invalid/@bob")
        }],
    }
}

/// Resolvable URLs are added, unresolvable ones counted as failures, and
/// already-seen ones left alone.
#[tokio::test]
async fn add_context_urls_counts_what_became_of_each_url() {
    let server = MockServer::start_async().await;
    let dir = common::tmp_dir();
    let config = common::test_config(&dir, "home.invalid");
    let db = db::open(&config).await.unwrap();
    let http = HttpClient::new(config.clone(), db.clone());
    let home = HomeServer::with_scheme(&host(&server), "http", "token", http.clone());
    let mut state = State::default();
    let mut ctx = runner::run_context(&config, &mut state, &http, &home, &db);

    // two URLs resolve, one does not, and two are already seen
    let ok_urls = [
        "https://remote.invalid/@a/1".to_string(),
        "https://remote.invalid/@a/2".to_string(),
    ];
    let bad_url = "https://remote.invalid/@a/3".to_string();
    let seen_urls = [
        "https://remote.invalid/@a/4".to_string(),
        "https://remote.invalid/@a/5".to_string(),
    ];
    ctx.state.seen_urls_mut().add(&seen_urls[0], None);
    ctx.state.seen_urls_mut().add(&seen_urls[1], None);
    for url in &ok_urls {
        let url = url.clone();
        server
            .mock_async(move |when, then| {
                when.method(Method::GET)
                    .path("/api/v2/search")
                    .query_param("q", url.as_str());
                then.status(200);
            })
            .await;
    }
    server
        .mock_async(|when, then| {
            when.method(Method::GET)
                .path("/api/v2/search")
                .query_param("q", bad_url.as_str());
            then.status(500);
        })
        .await;

    let logs = capture::LogCapture::new();
    let guard = logs.install();
    let mut urls = ok_urls.to_vec();
    urls.push(bad_url.clone());
    urls.extend(seen_urls.iter().cloned());
    add_context_urls(&mut ctx, urls).await;
    drop(guard);

    let logged = logs.text();
    assert!(
        logged.contains(
            "Added 2 new context toots (with 1 failures) from 5 context urls (2 already seen)"
        ),
        "the summary line says what became of the URLs: {logged}"
    );
    assert!(ctx.state.seen_urls().contains(&ok_urls[0]));
    assert!(ctx.state.seen_urls().contains(&ok_urls[1]));
    assert!(
        !ctx.state.seen_urls().contains(&bad_url),
        "a URL that did not resolve is not remembered"
    );
}

/// A reply's original post is remembered, so the same reply is not chased to
/// its server twice — and a reply recorded as unfindable stays that way.
#[tokio::test]
async fn a_replied_toot_is_answered_from_the_cache() {
    let dir = common::tmp_dir();
    let config = common::test_config(&dir, "home.invalid");
    let db = db::open(&config).await.unwrap();
    let http = HttpClient::new(config.clone(), db.clone());
    // the home server is pointed nowhere: any request the cache misses would
    // fail the test by trying a host that is not there
    let home = HomeServer::with_scheme("127.0.0.1:9", "http", "token", http.clone());
    let mut state = State::default();
    let mut ctx = runner::run_context(&config, &mut state, &http, &home, &db);

    // the cached answer for alice's reply to bob's status 9
    let entry = RepliedToot {
        url: "https://remote.invalid/@bob/9".to_string(),
        post_ref: PostRef::new("remote.invalid", "9"),
    };
    ctx.state.replied_toot_server_ids_mut().insert(
        "https://home.invalid/@bob/9".to_string(),
        Some(entry.clone()),
    );
    // and a reply recorded as unfindable
    ctx.state
        .replied_toot_server_ids_mut()
        .insert("https://home.invalid/@bob/8".to_string(), None);

    let found = replied_toot_server_ids(&mut ctx, &[reply_toot("9"), reply_toot("8")]).await;

    assert_eq!(found.len(), 1, "only the cached answer comes back");
    assert_eq!(found[0].url, entry.url);
    assert_eq!(found[0].post_ref.post_id, "9");
}

/// The mock server's nodeinfo, preloaded so the scan asks it nothing about
/// what it runs.
fn know_mock_server(state: &mut State, server_host: &str) {
    let known = fedifetcher::servers::ServerInfo::from_nodeinfo(
        server_host,
        &serde_json::json!({
            "software": {"name": "mastodon", "version": "4.3.0"},
            "protocols": ["activitypub"],
        }),
    )
    .unwrap();
    state.seen_hosts_mut().add(server_host, known.to_state());
}

/// The context of status `9` on the mock server: one ancestor, one
/// descendant on another server, and one on our own.
async fn mock_status_context(server: &MockServer, server_host: &str) {
    server
        .mock_async(move |when, then| {
            when.method(Method::GET).path("/robots.txt");
            then.status(200).body("User-agent: *\nAllow: /\n");
        })
        .await;
    server
        .mock_async(move |when, then| {
            when.method(Method::GET).path("/api/v1/statuses/9/context");
            then.status(200).json_body(serde_json::json!({
                "ancestors": [{ "url": format!("https://{server_host}/@bob/8") }],
                "descendants": [
                    { "url": format!("https://{server_host}/@carol/10") },
                    { "url": "https://home.invalid/@alice/1" },
                ],
            }));
        })
        .await;
}

/// A scan fetches a toot's context from the server that hosts it, records
/// the toot as a trigger post, and leaves out the context that lives on our
/// own server.
#[tokio::test]
async fn a_scan_fetches_a_toots_context_from_the_server_that_hosts_it() {
    let server = MockServer::start_async().await;
    let server_host = host(&server);
    let dir = common::tmp_dir();
    let config = common::test_config(&dir, "home.invalid");
    let db = db::open(&config).await.unwrap();
    let http = HttpClient::with_scheme(config.clone(), db.clone(), "http");
    let home = HomeServer::with_scheme("127.0.0.1:9", "http", "token", http.clone());
    let mut state = State::default();
    know_mock_server(&mut state, &server_host);
    let mut ctx = runner::run_context(&config, &mut state, &http, &home, &db);

    mock_status_context(&server, &server_host).await;
    let toot = reply_toot("9");
    let toot = Post {
        url: format!("https://{server_host}/@bob/9"),
        uri: format!("https://{server_host}/@bob/9"),
        ..toot
    };

    let logs = capture::LogCapture::new();
    let guard = logs.install();
    let found = known_context_urls(&mut ctx, &[toot], "scan", false).await;
    drop(guard);

    assert_eq!(
        found,
        vec![
            format!("https://{server_host}/@bob/8"),
            format!("https://{server_host}/@carol/10"),
        ],
        "the context on our own server is left out"
    );
    let logged = logs.text();
    assert!(
        logged.contains("1 on our server"),
        "the summary says what stayed local: {logged}"
    );
    let recorded = db::trigger_post(&db, &format!("https://{server_host}/@bob/9"))
        .await
        .unwrap();
    assert!(
        recorded.is_some(),
        "the scanned toot is recorded as a trigger post"
    );
}

/// A toot whose server cannot be asked is counted as unresolved, and the
/// scan moves on.
#[tokio::test]
async fn a_scan_counts_a_toot_its_server_cannot_answer_for() {
    let dir = common::tmp_dir();
    let config = common::test_config(&dir, "home.invalid");
    let db = db::open(&config).await.unwrap();
    let http = HttpClient::new(config.clone(), db.clone());
    let home = HomeServer::with_scheme("127.0.0.1:9", "http", "token", http.clone());
    let mut state = State::default();
    let mut ctx = runner::run_context(&config, &mut state, &http, &home, &db);

    let logs = capture::LogCapture::new();
    let guard = logs.install();
    let found = known_context_urls(&mut ctx, &[reply_toot("9")], "scan", false).await;
    drop(guard);

    assert!(found.is_empty(), "nothing comes back for an unknown server");
    assert!(
        logs.text().contains("1 unresolved"),
        "the summary says the toot was unresolved: {}",
        logs.text()
    );
}

/// A toot checked recently is not fetched from again.
#[tokio::test]
async fn a_toot_checked_recently_is_not_fetched_again() {
    let server = MockServer::start_async().await;
    let server_host = host(&server);
    let dir = common::tmp_dir();
    let config = common::test_config(&dir, "home.invalid");
    let db = db::open(&config).await.unwrap();
    let http = HttpClient::with_scheme(config.clone(), db.clone(), "http");
    let home = HomeServer::with_scheme("127.0.0.1:9", "http", "token", http.clone());
    let mut state = State::default();
    know_mock_server(&mut state, &server_host);
    let mut ctx = runner::run_context(&config, &mut state, &http, &home, &db);

    mock_status_context(&server, &server_host).await;
    let toot = Post {
        url: format!("https://{server_host}/@bob/9"),
        uri: format!("https://{server_host}/@bob/9"),
        ..reply_toot("9")
    };
    ctx.state
        .recently_checked_context_mut()
        .mark_fetched(&toot.uri, toot.created_at);

    let logs = capture::LogCapture::new();
    let guard = logs.install();
    let found = known_context_urls(&mut ctx, &[toot], "scan", false).await;
    drop(guard);

    assert!(found.is_empty(), "nothing is fetched again");
    assert!(
        logs.text().contains("1 already checked recently"),
        "the summary says why: {}",
        logs.text()
    );
}

/// A reply is chased to the toot it replied to — through the redirect the
/// home server's URL shape hands out — and remembered, so the same reply is
/// never chased twice. A reply the chase cannot resolve is remembered as
/// unfindable.
#[tokio::test]
async fn a_reply_is_chased_to_the_server_hosting_the_toot_it_replied_to() {
    let server = MockServer::start_async().await;
    let server_host = host(&server);
    let dir = common::tmp_dir();
    // our own server is the mock: the replied-toot URL is spelled with it
    let config = common::test_config(&dir, &server_host);
    let db = db::open(&config).await.unwrap();
    let http = HttpClient::with_scheme(config.clone(), db.clone(), "http");
    let home = HomeServer::with_scheme("127.0.0.1:9", "http", "token", http.clone());
    let mut state = State::default();
    know_mock_server(&mut state, &server_host);
    let mut ctx = runner::run_context(&config, &mut state, &http, &home, &db);

    // the replied-to toot's URL redirects to where the post really lives
    let redirect_host = server_host.clone();
    server
        .mock_async(move |when, then| {
            when.method(Method::HEAD).path("/@bob/9");
            then.status(302)
                .header("Location", format!("https://{redirect_host}/@bob/9"));
        })
        .await;
    // an object URL whose redirect goes nowhere readable
    server
        .mock_async(|when, then| {
            when.method(Method::HEAD).path("/@bob/8");
            then.status(302).header("Location", "not a url");
        })
        .await;

    // alice's reply to bob's status 9 names bob as the mention it replies to;
    // a second reply carries no reply id, and a third cannot be resolved
    let mut resolvable = reply_toot("9");
    resolvable.url = format!("https://{server_host}/@alice/1");
    resolvable.uri = resolvable.url.clone();
    resolvable.account = Some(User::new("alice", format!("https://{server_host}/@alice")));

    let mut no_reply_id = reply_toot("7");
    no_reply_id.url = format!("https://{server_host}/@alice/2");
    no_reply_id.uri = no_reply_id.url.clone();
    no_reply_id.in_reply_to_id = None;
    no_reply_id.account = Some(User::new("alice", format!("https://{server_host}/@alice")));

    let mut unresolvable = reply_toot("8");
    unresolvable.url = format!("https://{server_host}/@alice/3");
    unresolvable.uri = unresolvable.url.clone();
    unresolvable.account = Some(User::new("alice", format!("https://{server_host}/@alice")));

    let found = replied_toot_server_ids(&mut ctx, &[resolvable, no_reply_id, unresolvable]).await;

    assert_eq!(found.len(), 1, "only the resolvable reply comes back");
    assert_eq!(found[0].post_ref.server, server_host);
    assert_eq!(found[0].post_ref.post_id, "9");
    // the unresolved reply is remembered as unfindable, so it is not asked
    // about again
    assert_eq!(
        ctx.state
            .replied_toot_server_ids()
            .get(&format!("http://{server_host}/@bob/8")),
        Some(&None),
        "the unresolvable reply is remembered"
    );
}

/// The context of the toots our replies replied to is fetched from their own
/// servers, leaving out what lives on ours.
#[tokio::test]
async fn the_replied_toots_context_comes_from_its_own_server() {
    let server = MockServer::start_async().await;
    let server_host = host(&server);
    let dir = common::tmp_dir();
    let config = common::test_config(&dir, "home.invalid");
    let db = db::open(&config).await.unwrap();
    let http = HttpClient::with_scheme(config.clone(), db.clone(), "http");
    let home = HomeServer::with_scheme("127.0.0.1:9", "http", "token", http.clone());
    let mut state = State::default();
    know_mock_server(&mut state, &server_host);
    let mut ctx = runner::run_context(&config, &mut state, &http, &home, &db);

    mock_status_context(&server, &server_host).await;
    let entry = RepliedToot {
        url: format!("https://{server_host}/@bob/9"),
        post_ref: PostRef::new(server_host.clone(), "9"),
    };

    let urls = context_urls(&mut ctx, vec![entry]).await;

    assert_eq!(
        urls,
        vec![
            format!("https://{server_host}/@bob/8"),
            format!("https://{server_host}/@carol/10"),
        ],
        "the context on our own server is left out"
    );
}
