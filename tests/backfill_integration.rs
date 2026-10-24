//! Backfilling one account: the opt-out rules, and that skipping an account
//! is logged with the reason.

mod common;

use fedifetcher::api::mastodon::HomeServer;
use fedifetcher::backfill::{UserSet, user_posts};
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
    let mut ctx = common::run_context(&config, &mut state, &client, &home, &db);

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

    let logs = common::LogCapture::new();
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
    let mut ctx = common::run_context(&config, &mut state, &client, &home, &db);

    // A local user: no fetching happens either way, but with the rule off
    // the opt-out check is skipped and the account is recorded as checked.
    let mut user = User::new("bot", "https://example.social/@bot");
    user.note = "beep boop, nobot".to_string();

    let logs = common::LogCapture::new();
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
    let mut ctx = common::run_context(&config, &mut state, &client, &home, &db);

    // A local user: no fetching happens either way, but with the force the
    // opt-out check is skipped and the account is recorded as checked.
    let mut user = User::new("bot", "https://example.social/@bot");
    user.note = "beep boop, nobot".to_string();

    let logs = common::LogCapture::new();
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
    let mut ctx = common::run_context(&config, &mut state, &client, &home, &db);

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

    let logs = common::LogCapture::new();
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
    let mut ctx = common::run_context(&config, &mut state, &client, &home, &db);

    let user = User::new("alice", "not a url");

    let logs = common::LogCapture::new();
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
    let mut ctx = common::run_context(&config, &mut state, &client, &home, &db);

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
    let mut ctx = common::run_context(&config, &mut state, &client, &home, &db);

    // not one of the Mastodon client's profile shapes
    let user = User::new(
        "alice@remote.invalid",
        "https://remote.invalid/profiles/alice",
    );

    let logs = common::LogCapture::new();
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
    let mut ctx = common::run_context(&config, &mut state, &client, &home, &db);

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
