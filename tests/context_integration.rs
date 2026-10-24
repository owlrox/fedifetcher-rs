//! The context scan's outputs: which context URLs get added to the server,
//! and which replies to a reply are followed back to their own server.

mod common;

use fedifetcher::api::mastodon::HomeServer;
use fedifetcher::context::{add_context_urls, replied_toot_server_ids};
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
    let mut ctx = common::run_context(&config, &mut state, &http, &home, &db);

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

    let logs = common::LogCapture::new();
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
    let mut ctx = common::run_context(&config, &mut state, &http, &home, &db);

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
