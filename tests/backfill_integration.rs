//! Backfilling one account: the opt-out rules, and that skipping an account
//! is logged with the reason.

mod capture;
mod common;
mod runner;

use fedifetcher::api::mastodon::HomeServer;
use fedifetcher::backfill::{Force, UserSet, add_user_posts, user_posts};
use fedifetcher::http::HttpClient;
use fedifetcher::posts::Post;
use fedifetcher::store::State;
use fedifetcher::users::User;
use httpmock::Method;
use httpmock::prelude::*;

#[tokio::test]
async fn an_account_that_opted_out_is_logged_and_not_fetched() {
    let server = MockServer::start_async().await;
    let dir = common::tmp_dir();
    let config = common::test_config(&dir, "example.social");
    let db = fedifetcher::db::open(&config).await.unwrap();
    let client = HttpClient::new(config.clone(), db.clone());
    let mut state = State::default();
    let home = HomeServer::new("example.social", "secret-token", client.clone());
    let mut ctx = runner::run_context(&config, &mut state, &client, &home, &db);

    // the first things a fetch would ask for, if the skip did not happen
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

    let mut user = User::new("bot", format!("{}/@bot", server.base_url()));
    user.note = "beep boop, nobot".to_string();

    let logs = capture::LogCapture::new();
    let guard = logs.install();
    let posts = user_posts(&mut ctx, &user, UserSet::RecentlyCheckedUsers, false).await;
    drop(guard);

    assert!(posts.is_none(), "an opted-out account is skipped");
    assert_eq!(nodeinfo.calls(), 0, "nothing is fetched for the account");
    assert_eq!(robots.calls(), 0, "nothing is fetched for the account");
    let logged = logs.text();
    let line = logged
        .lines()
        .find(|line| line.contains("opted out"))
        .unwrap_or_else(|| panic!("the skip should be logged, got:\n{logged}"));
    assert!(
        line.contains("bot"),
        "the log line names the account: {line}"
    );
    assert!(
        line.contains("its profile asks not to be crawled (nobot)"),
        "the log line gives the reason: {line}"
    );
    println!("logged: {line}");
}

#[tokio::test]
async fn an_account_that_opted_out_is_fetched_when_the_rule_is_off() {
    let dir = common::tmp_dir();
    let mut config = common::test_config(&dir, "example.social");
    config.respect_user_opt_out = false;
    let db = fedifetcher::db::open(&config).await.unwrap();
    let client = HttpClient::new(config.clone(), db.clone());
    let mut state = State::default();
    let home = HomeServer::new("example.social", "secret-token", client.clone());
    let mut ctx = runner::run_context(&config, &mut state, &client, &home, &db);

    // A local user: no fetching happens either way, but with the rule off
    // the opt-out check is skipped and the account is recorded as checked.
    let mut user = User::new("bot", "https://example.social/@bot");
    user.note = "beep boop, nobot".to_string();

    let logs = capture::LogCapture::new();
    let guard = logs.install();
    let posts = user_posts(&mut ctx, &user, UserSet::RecentlyCheckedUsers, false).await;
    drop(guard);

    assert!(posts.is_none(), "a local user has no posts to fetch");
    assert!(
        !logs.text().contains("opted out"),
        "no opt-out is logged when the rule is off: {}",
        logs.text()
    );
    assert!(
        state.recently_checked_users().contains("bot"),
        "the account is recorded as checked"
    );
}

/// The skip costs no requests on any run, the account is recorded in the
/// database with its reason, and the log line fires the first time only:
/// LogCapture sees INFO and above, so a second line would mean the repeat
/// skip was logged at INFO again.
/// The data page's button on an opted-out account forces the fetch: the
/// opt-out check is bypassed, the same way the config rule's being off
/// bypasses it.
#[tokio::test]
async fn an_account_that_opted_out_is_fetched_when_forced() {
    let dir = common::tmp_dir();
    let config = common::test_config(&dir, "example.social");
    let db = fedifetcher::db::open(&config).await.unwrap();
    let client = HttpClient::new(config.clone(), db.clone());
    let mut state = State::default();
    let home = HomeServer::new("example.social", "secret-token", client.clone());
    let mut ctx = runner::run_context(&config, &mut state, &client, &home, &db);

    // A local user: no fetching happens either way, but with the force the
    // opt-out check is skipped and the account is recorded as checked.
    let mut user = User::new("bot", "https://example.social/@bot");
    user.note = "beep boop, nobot".to_string();

    let logs = capture::LogCapture::new();
    let guard = logs.install();
    let posts = user_posts(&mut ctx, &user, UserSet::RecentlyCheckedUsers, true).await;
    drop(guard);

    assert!(posts.is_none(), "a local user has no posts to fetch");
    assert!(
        !logs.text().contains("opted out"),
        "no opt-out is logged when the fetch is forced: {}",
        logs.text()
    );
    assert!(
        state.recently_checked_users().contains("bot"),
        "the account is recorded as checked"
    );
}

#[tokio::test]
async fn an_opted_out_account_is_recorded_and_logged_once() {
    let server = MockServer::start_async().await;
    let dir = common::tmp_dir();
    let config = common::test_config(&dir, "example.social");
    let db = fedifetcher::db::open(&config).await.unwrap();
    let client = HttpClient::new(config.clone(), db.clone());
    let mut state = State::default();
    let home = HomeServer::new("example.social", "secret-token", client.clone());
    let mut ctx = runner::run_context(&config, &mut state, &client, &home, &db);

    // the first things a fetch would ask for, if the skip did not happen
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

    let mut user = User::new("bot", format!("{}/@bot", server.base_url()));
    user.indexable = false;

    let logs = capture::LogCapture::new();
    let guard = logs.install();
    // two runs over the same database
    let first = user_posts(&mut ctx, &user, UserSet::RecentlyCheckedUsers, false).await;
    let second = user_posts(&mut ctx, &user, UserSet::RecentlyCheckedUsers, false).await;
    drop(guard);

    assert!(
        first.is_none() && second.is_none(),
        "both runs skip the account"
    );
    assert_eq!(
        nodeinfo.calls(),
        0,
        "no run fetches the account's server info"
    );
    assert_eq!(robots.calls(), 0, "no run fetches the account's robots.txt");

    let logged = logs.text();
    let skip_lines: Vec<&str> = logged
        .lines()
        .filter(|line| line.contains("opted out"))
        .collect();
    assert_eq!(
        skip_lines.len(),
        1,
        "the skip is logged once, not once per run:\n{logged}"
    );
    assert!(skip_lines[0].contains("bot"), "the line names the account");

    assert_eq!(
        fedifetcher::db::opted_out_account_reason(&db, "bot")
            .await
            .unwrap(),
        Some("it is not indexable".to_string()),
        "the account is recorded with its reason"
    );
}

/// An account whose profile URL cannot be read is recorded as checked, so
/// it is not retried every run.
#[tokio::test]
async fn an_unparseable_profile_url_is_recorded_as_checked() {
    let dir = common::tmp_dir();
    let config = common::test_config(&dir, "example.social");
    let db = fedifetcher::db::open(&config).await.unwrap();
    let client = HttpClient::new(config.clone(), db.clone());
    let mut state = State::default();
    let home = HomeServer::new("example.social", "secret-token", client.clone());
    let mut ctx = runner::run_context(&config, &mut state, &client, &home, &db);

    let user = User::new("alice", "not a url");

    let logs = capture::LogCapture::new();
    let guard = logs.install();
    let posts = user_posts(&mut ctx, &user, UserSet::RecentlyCheckedUsers, false).await;
    drop(guard);

    assert!(posts.is_none(), "nothing can be fetched for the account");
    assert!(
        logs.text().contains("Error parsing Profile URL not a url"),
        "the unreadable URL is logged: {}",
        logs.text()
    );
    assert!(
        state.recently_checked_users().contains("alice"),
        "the account is recorded as checked"
    );
}

/// An account on a server that speaks none of our APIs is not fetched, and
/// not recorded either: nothing about it was learned.
#[tokio::test]
async fn a_server_with_no_known_api_is_not_backfilled() {
    let dir = common::tmp_dir();
    let config = common::test_config(&dir, "example.social");
    let db = fedifetcher::db::open(&config).await.unwrap();
    let client = HttpClient::new(config.clone(), db.clone());
    let mut state = State::default();
    // the server is already known, so no lookup happens
    let known = fedifetcher::servers::ServerInfo::from_nodeinfo(
        "remote.invalid",
        &serde_json::json!({"software": {"name": "something-new", "version": "1"}}),
    )
    .unwrap();
    state
        .seen_hosts_mut()
        .add("remote.invalid", known.to_state());
    let home = HomeServer::new("example.social", "secret-token", client.clone());
    let mut ctx = runner::run_context(&config, &mut state, &client, &home, &db);

    let user = User::new("alice@remote.invalid", "https://remote.invalid/@alice");

    let posts = user_posts(&mut ctx, &user, UserSet::RecentlyCheckedUsers, false).await;

    assert!(
        posts.is_none(),
        "a server we cannot talk to yields no posts"
    );
    assert!(
        !state
            .recently_checked_users()
            .contains("alice@remote.invalid"),
        "nothing was learned about the account, so it is not recorded"
    );
}

/// An account on a known server whose profile URL the server's client cannot
/// read is recorded as checked, like an unparseable one.
#[tokio::test]
async fn a_profile_shape_the_client_cannot_read_is_recorded_as_checked() {
    let dir = common::tmp_dir();
    let config = common::test_config(&dir, "example.social");
    let db = fedifetcher::db::open(&config).await.unwrap();
    let client = HttpClient::new(config.clone(), db.clone());
    let mut state = State::default();
    let known = fedifetcher::servers::ServerInfo::from_nodeinfo(
        "remote.invalid",
        &serde_json::json!({"software": {"name": "mastodon", "version": "4"}}),
    )
    .unwrap();
    state
        .seen_hosts_mut()
        .add("remote.invalid", known.to_state());
    let home = HomeServer::new("example.social", "secret-token", client.clone());
    let mut ctx = runner::run_context(&config, &mut state, &client, &home, &db);

    // not one of the Mastodon client's profile shapes
    let user = User::new(
        "alice@remote.invalid",
        "https://remote.invalid/profiles/alice",
    );

    let logs = capture::LogCapture::new();
    let guard = logs.install();
    let posts = user_posts(&mut ctx, &user, UserSet::RecentlyCheckedUsers, false).await;
    drop(guard);

    assert!(posts.is_none(), "nothing can be fetched for the account");
    assert!(
        logs.text().contains("Error parsing Profile URL"),
        "the unreadable URL is logged: {}",
        logs.text()
    );
    assert!(
        state
            .recently_checked_users()
            .contains("alice@remote.invalid"),
        "the account is recorded as checked"
    );
}

/// A post with no context to fetch is resolved on our server and remembered,
/// without any context being asked for.
#[tokio::test]
async fn a_post_without_possible_context_is_resolved_and_remembered() {
    let server = MockServer::start_async().await;
    let dir = common::tmp_dir();
    let config = common::test_config(&dir, "example.social");
    let db = fedifetcher::db::open(&config).await.unwrap();
    let client = HttpClient::new(config.clone(), db.clone());
    let home = HomeServer::with_scheme(
        server.base_url().trim_start_matches("http://"),
        "http",
        "secret-token",
        client.clone(),
    );
    let mut state = State::default();
    let mut ctx = runner::run_context(&config, &mut state, &client, &home, &db);

    let url = "https://remote.example/@alice/1";
    let search = server
        .mock_async(move |when, then| {
            when.method(Method::GET)
                .path("/api/v2/search")
                .query_param("q", url);
            then.status(200);
        })
        .await;

    // no reply count and no reply: there is no context to ask for
    let post = Post {
        url: url.to_string(),
        uri: url.to_string(),
        created_at: fedifetcher::timeutil::now(),
        is_public: true,
        reblog: None,
        in_reply_to_id: None,
        in_reply_to_account_id: None,
        reply_count: None,
        account: Some(User::new(
            "alice@remote.example",
            "https://remote.example/@alice",
        )),
        mentions: vec![],
    };

    let added = fedifetcher::backfill::add_post_with_context(&mut ctx, &post).await;

    assert!(added, "the post resolves on our server");
    assert_eq!(search.calls(), 1, "the post itself is resolved");
    assert!(
        state.seen_urls().contains(url),
        "the post is remembered as seen"
    );
}

/// The mock server's host, without the scheme.
fn host(server: &MockServer) -> String {
    server.base_url().trim_start_matches("http://").to_string()
}

/// The mock server already knows alice and answers her statuses with one
/// post, `http://{host}/@alice/10`.
async fn mock_alice(server: &MockServer) {
    let host = host(server);
    server
        .mock_async(move |when, then| {
            when.method(Method::GET).path("/robots.txt");
            then.status(200).body("User-agent: *\nAllow: /\n");
        })
        .await;
    server
        .mock_async(move |when, then| {
            when.method(Method::GET)
                .path("/api/v1/accounts/lookup")
                .query_param("acct", "alice");
            then.status(200).json_body(serde_json::json!({ "id": "1" }));
        })
        .await;
    server
        .mock_async(move |when, then| {
            when.method(Method::GET).path("/api/v1/accounts/1/statuses");
            then.status(200).json_body(serde_json::json!([{
                "id": "10",
                "url": format!("https://{host}/@alice/10"),
                "uri": format!("https://{host}/@alice/10"),
                "created_at": "2026-10-22T10:00:00.000Z",
                "visibility": "public",
                "reblog": null,
                "replies_count": 2,
            }]));
        })
        .await;
}

/// The mock server's host, preloaded as a known mastodon server so no
/// discovery request is made.
fn known_mock(server: &MockServer) -> fedifetcher::servers::ServerInfo {
    fedifetcher::servers::ServerInfo::from_nodeinfo(
        &host(server),
        &serde_json::json!({
            "software": {"name": "mastodon", "version": "4.3.0"},
            "protocols": ["activitypub"],
        }),
    )
    .unwrap()
}

/// A post the home server cannot resolve counts as a failure: the account is
/// left unmarked, so the next run tries it again.
#[tokio::test]
async fn a_post_the_home_server_cannot_resolve_counts_as_failed() {
    let server = MockServer::start_async().await;
    let home_server = MockServer::start_async().await;
    let dir = common::tmp_dir();
    let config = common::test_config(&dir, "example.social");
    let db = fedifetcher::db::open(&config).await.unwrap();
    let client = HttpClient::with_scheme(config.clone(), db.clone(), "http");
    let mut state = State::default();
    state
        .seen_hosts_mut()
        .add(&host(&server), known_mock(&server).to_state());
    let home = HomeServer::with_scheme(&host(&home_server), "http", "secret-token", client.clone());
    let mut ctx = runner::run_context(&config, &mut state, &client, &home, &db);

    mock_alice(&server).await;
    home_server
        .mock_async(|when, then| {
            when.method(Method::GET).path("/api/v2/search");
            then.status(500);
        })
        .await;

    let logs = capture::LogCapture::new();
    let guard = logs.install();
    add_user_posts(
        &mut ctx,
        vec![User::new(
            "alice",
            format!("https://{}/@alice", host(&server)),
        )],
        UserSet::RecentlyCheckedUsers,
        Force::default(),
    )
    .await;
    drop(guard);

    let logged = logs.text();
    assert!(
        logged.contains("Added 0 posts for user alice with 1 errors"),
        "the failure is counted: {logged}"
    );
    assert!(
        !state.recently_checked_users().contains("alice"),
        "an account with failures is tried again next run"
    );
    assert!(!state.all_known_users().contains("alice"));
}

/// A post the home server resolves gets its context fetched from the server
/// that hosts it, and the account is recorded as known.
#[tokio::test]
async fn a_resolved_posts_context_is_added_during_backfill() {
    let server = MockServer::start_async().await;
    let home_server = MockServer::start_async().await;
    let dir = common::tmp_dir();
    let mut config = common::test_config(&dir, "example.social");
    config.backfill_with_context = true;
    let db = fedifetcher::db::open(&config).await.unwrap();
    let client = HttpClient::with_scheme(config.clone(), db.clone(), "http");
    let mut state = State::default();
    state
        .seen_hosts_mut()
        .add(&host(&server), known_mock(&server).to_state());
    let home = HomeServer::with_scheme(&host(&home_server), "http", "secret-token", client.clone());
    let mut ctx = runner::run_context(&config, &mut state, &client, &home, &db);

    mock_alice(&server).await;
    let mock_host = host(&server);
    let ctx_host = mock_host.clone();
    server
        .mock_async(move |when, then| {
            when.method(Method::GET).path("/api/v1/statuses/10/context");
            then.status(200).json_body(serde_json::json!({
                "ancestors": [{ "url": format!("https://{ctx_host}/@bob/9") }],
                "descendants": [{ "url": format!("https://{ctx_host}/@carol/11") }],
            }));
        })
        .await;
    // the home server resolves everything the scan brings it
    home_server
        .mock_async(|when, then| {
            when.method(Method::GET).path("/api/v2/search");
            then.status(200);
        })
        .await;

    let logs = capture::LogCapture::new();
    let guard = logs.install();
    add_user_posts(
        &mut ctx,
        vec![User::new(
            "alice",
            format!("https://{}/@alice", host(&server)),
        )],
        UserSet::RecentlyCheckedUsers,
        Force::default(),
    )
    .await;
    drop(guard);

    let logged = logs.text();
    assert!(
        logged.contains("Added 1 posts for user alice with 0 errors"),
        "the post is added: {logged}"
    );
    assert!(
        logged.contains("Added 2 new context toots"),
        "the post's context is added: {logged}"
    );
    assert!(
        state
            .seen_urls()
            .contains(&format!("https://{mock_host}/@bob/9"))
            && state
                .seen_urls()
                .contains(&format!("https://{mock_host}/@carol/11")),
        "the context urls are remembered as seen"
    );
    assert!(
        state.recently_checked_users().contains("alice")
            && state.all_known_users().contains("alice"),
        "a fully fetched account is recorded as known"
    );
}

/// An account on a server that cannot be asked what it runs is skipped, and
/// the skip says which server failed.
#[tokio::test]
async fn an_account_on_an_unreachable_server_is_skipped() {
    let dir = common::tmp_dir();
    let config = common::test_config(&dir, "example.social");
    let db = fedifetcher::db::open(&config).await.unwrap();
    let client = HttpClient::new(config.clone(), db.clone());
    let mut state = State::default();
    let home = HomeServer::new("example.social", "secret-token", client.clone());
    let mut ctx = runner::run_context(&config, &mut state, &client, &home, &db);

    let user = User::new("bob", "https://127.0.0.1:9/@bob");

    let logs = capture::LogCapture::new();
    let guard = logs.install();
    let posts = user_posts(&mut ctx, &user, UserSet::RecentlyCheckedUsers, false).await;
    drop(guard);

    assert!(posts.is_none(), "nothing can be fetched");
    assert!(
        logs.text()
            .contains("server 127.0.0.1:9 not found for post"),
        "the failed lookup is logged: {}",
        logs.text()
    );
}

/// An account whose profile URL the server's own flavor cannot read is
/// recorded in the target set anyway, so it is not retried every run.
#[tokio::test]
async fn a_profile_url_the_flavor_cannot_read_is_recorded_as_checked() {
    let dir = common::tmp_dir();
    let config = common::test_config(&dir, "example.social");
    let db = fedifetcher::db::open(&config).await.unwrap();
    let client = HttpClient::new(config.clone(), db.clone());
    let mut state = State::default();
    // the server is known to speak mastodon, but the URL is a Lemmy
    // community's, which no mastodon URL shape matches
    let known = fedifetcher::servers::ServerInfo::from_nodeinfo(
        "remote.invalid",
        &serde_json::json!({"software": {"name": "mastodon", "version": "1"}}),
    )
    .unwrap();
    state
        .seen_hosts_mut()
        .add("remote.invalid", known.to_state());
    let home = HomeServer::new("example.social", "secret-token", client.clone());
    let mut ctx = runner::run_context(&config, &mut state, &client, &home, &db);

    let user = User::new("alice@remote.invalid", "https://remote.invalid/c/community");

    let logs = capture::LogCapture::new();
    let guard = logs.install();
    let posts = user_posts(&mut ctx, &user, UserSet::KnownFollowings, false).await;
    drop(guard);

    assert!(posts.is_none(), "no URL shape means no posts");
    assert!(
        logs.text()
            .contains("Error parsing Profile URL https://remote.invalid/c/community"),
        "the unreadable URL is logged: {}",
        logs.text()
    );
    assert!(
        state.known_followings().contains("alice@remote.invalid"),
        "the account is recorded in the set it was backfilled for"
    );
}
