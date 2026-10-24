//! The tasks a run does: that each runs only when its setting asks for it,
//! that a canceled run stops between tasks, and how replies are picked.

mod common;

use fedifetcher::api::mastodon::HomeServer;
use fedifetcher::db;
use fedifetcher::http::HttpClient;
use fedifetcher::store::State;
use fedifetcher::tasks::{self, fetch_timeline_context, reply_toots};
use fedifetcher::timeutil;
use httpmock::Method;
use httpmock::prelude::*;
use serde_json::{Value, json};
use time::Duration as TimeDuration;

/// The mock server's host, without the scheme.
fn host(server: &MockServer) -> String {
    server.base_url().trim_start_matches("http://").to_string()
}

/// An endpoint on the mock server that answers with an empty JSON array.
async fn empty_array<'a>(server: &'a MockServer, path: &str) -> httpmock::Mock<'a> {
    let path = path.to_string();
    server
        .mock_async(move |when, then| {
            when.method(Method::GET).path(path.as_str());
            then.status(200).json_body(json!([]));
        })
        .await
}

/// A user on the mock home server, as the API spells it.
fn account(acct: &str, url: &str) -> Value {
    json!({
        "id": "5",
        "acct": acct,
        "url": url,
        "indexable": true,
        "discoverable": true,
    })
}

/// A status as the home server's API spells it, replying to status
/// `reply_to`.
fn reply_status(id: &str, reply_to: &str, url: &str, age: TimeDuration) -> Value {
    json!({
        "id": id,
        "in_reply_to_id": reply_to,
        "in_reply_to_account_id": "7",
        "url": url,
        "uri": url,
        "created_at": timeutil::fmt_state_ts(timeutil::now() - age),
        "visibility": "public",
        "replies_count": 1,
        "account": account("alice", "https://home.invalid/@alice"),
        "mentions": [account("bob", "https://home.invalid/@bob")],
    })
}

#[tokio::test]
async fn each_task_runs_when_its_setting_asks_for_it() {
    let server = MockServer::start_async().await;
    let dir = common::tmp_dir();
    let mut config = common::test_config(&dir, "home.invalid");
    config.from_lists = true;
    config.max_list_length = 2;
    config.max_list_accounts = 2;
    config.reply_interval_in_hours = 1;
    config.home_timeline_length = 2;
    config.max_followings = 2;
    config.max_followers = 2;
    config.max_follow_requests = 2;
    config.from_notifications = 1;
    config.max_bookmarks = 2;
    config.max_favorites = 2;
    let db = db::open(&config).await.unwrap();
    let http = HttpClient::new(config.clone(), db.clone());
    let home = HomeServer::with_scheme(&host(&server), "http", "token", http.clone());
    let mut state = State::default();
    let mut ctx = common::run_context(&config, &mut state, &http, &home, &db);

    let lists = server
        .mock_async(|when, then| {
            when.method(Method::GET).path("/api/v1/lists");
            then.status(200).json_body(json!([
                {"id": "1", "title": "Friends"}
            ]));
        })
        .await;
    let list_timeline = server
        .mock_async(|when, then| {
            when.method(Method::GET)
                .path("/api/v1/timelines/list/1")
                .query_param("limit", "2");
            then.status(200).json_body(json!([]));
        })
        .await;
    let list_accounts = server
        .mock_async(|when, then| {
            when.method(Method::GET)
                .path("/api/v1/lists/1/accounts")
                .query_param("limit", "2");
            then.status(200).json_body(json!([]));
        })
        .await;
    let admin_accounts = server
        .mock_async(|when, then| {
            when.method(Method::GET).path("/api/v1/admin/accounts");
            then.status(200).json_body(json!([
                {
                    "id": "42",
                    "username": "alice",
                    "account": {"last_status_at": timeutil::fmt_state_ts(timeutil::now())},
                }
            ]));
        })
        .await;
    let account_statuses = server
        .mock_async(|when, then| {
            when.method(Method::GET)
                .path("/api/v1/accounts/42/statuses");
            then.status(200).json_body(json!([reply_status(
                "10",
                "9",
                "https://unreach.invalid/@alice/10",
                TimeDuration::minutes(5),
            )]));
        })
        .await;
    let verify = server
        .mock_async(|when, then| {
            when.method(Method::GET)
                .path("/api/v1/accounts/verify_credentials");
            then.status(200).json_body(json!({"id": "42"}));
        })
        .await;
    let timeline = empty_array(&server, "/api/v1/timelines/home").await;
    let following = empty_array(&server, "/api/v1/accounts/42/following").await;
    let followers = empty_array(&server, "/api/v1/accounts/42/followers").await;
    let follow_requests = empty_array(&server, "/api/v1/follow_requests").await;
    let notifications = empty_array(&server, "/api/v1/notifications").await;
    let bookmarks = empty_array(&server, "/api/v1/bookmarks").await;
    let favorites = empty_array(&server, "/api/v1/favourites").await;

    let logs = common::LogCapture::new();
    let guard = logs.install();
    tasks::run_enabled_tasks(&mut ctx)
        .await
        .expect("a run over an empty timeline should succeed");
    drop(guard);

    for (name, mock) in [
        ("lists", &lists),
        ("list timeline", &list_timeline),
        ("list accounts", &list_accounts),
        ("admin accounts", &admin_accounts),
        ("account statuses", &account_statuses),
        ("verify credentials", &verify),
        ("home timeline", &timeline),
        ("following", &following),
        ("followers", &followers),
        ("follow requests", &follow_requests),
        ("notifications", &notifications),
        ("bookmarks", &bookmarks),
        ("favorites", &favorites),
    ] {
        assert!(mock.calls() > 0, "the {name} task should have run");
    }
    let logged = logs.text();
    assert!(
        logged.contains("Found 1 reply toots"),
        "the replies are counted: {logged}"
    );
    assert!(
        logged.contains("Found 0 toots in list Friends"),
        "the list timeline is counted: {logged}"
    );
    assert!(
        logged.contains("Got 0 followings, 0 of which are new"),
        "the followings are counted: {logged}"
    );
}

/// Only the home timeline is enabled: no other task's endpoint is asked for
/// (each would answer 404 from the mock server and fail the run), and the
/// run succeeds on that one task alone.
#[tokio::test]
async fn a_task_whose_setting_is_off_does_not_run() {
    let server = MockServer::start_async().await;
    let dir = common::tmp_dir();
    let mut config = common::test_config(&dir, "home.invalid");
    config.home_timeline_length = 5;
    let db = db::open(&config).await.unwrap();
    let http = HttpClient::new(config.clone(), db.clone());
    let home = HomeServer::with_scheme(&host(&server), "http", "token", http.clone());
    let mut state = State::default();
    let mut ctx = common::run_context(&config, &mut state, &http, &home, &db);

    let timeline = server
        .mock_async(|when, then| {
            when.method(Method::GET)
                .path("/api/v1/timelines/home")
                .query_param("limit", "5");
            then.status(200).json_body(json!([]));
        })
        .await;

    tasks::run_enabled_tasks(&mut ctx)
        .await
        .expect("a run with one task enabled should succeed");

    assert_eq!(timeline.calls(), 1);
}

/// A run canceled before it starts finishes its first task and stops at the
/// checkpoint after it: nothing further is asked for, and the run reports
/// success rather than an error.
#[tokio::test]
async fn a_canceled_run_stops_at_the_checkpoint_after_the_first_task() {
    let server = MockServer::start_async().await;
    let dir = common::tmp_dir();
    let mut config = common::test_config(&dir, "home.invalid");
    config.from_lists = true;
    config.home_timeline_length = 5;
    let db = db::open(&config).await.unwrap();
    let http = HttpClient::new(config.clone(), db.clone());
    let home = HomeServer::with_scheme(&host(&server), "http", "token", http.clone());
    let mut state = State::default();
    let mut ctx = common::run_context(&config, &mut state, &http, &home, &db);
    ctx.cancel.cancel();

    let lists = server
        .mock_async(|when, then| {
            when.method(Method::GET).path("/api/v1/lists");
            then.status(200).json_body(json!([]));
        })
        .await;
    let timeline = server
        .mock_async(|when, then| {
            when.method(Method::GET).path("/api/v1/timelines/home");
            then.status(200).json_body(json!([]));
        })
        .await;

    tasks::run_enabled_tasks(&mut ctx)
        .await
        .expect("a stopped run is not a failure");

    assert_eq!(
        lists.calls(),
        1,
        "the first task runs before the checkpoint"
    );
    assert_eq!(
        timeline.calls(),
        0,
        "nothing after the checkpoint is asked for"
    );
}

/// Only replies by other users, within the interval, that we have not seen
/// are reply toots.
#[tokio::test]
async fn reply_toots_keeps_only_recent_unseen_replies() {
    let server = MockServer::start_async().await;
    let dir = common::tmp_dir();
    let mut config = common::test_config(&dir, "home.invalid");
    config.reply_interval_in_hours = 1;
    let db = db::open(&config).await.unwrap();
    let http = HttpClient::new(config.clone(), db.clone());
    let home = HomeServer::with_scheme(&host(&server), "http", "token", http.clone());
    let mut state = State::default();
    let mut ctx = common::run_context(&config, &mut state, &http, &home, &db);

    // a reply we have already seen
    ctx.state
        .seen_urls_mut()
        .add("https://home.invalid/@alice/7", None);

    server
        .mock_async(|when, then| {
            when.method(Method::GET)
                .path("/api/v1/accounts/42/statuses");
            then.status(200).json_body(json!([
                // a fresh reply to someone else
                reply_status(
                    "10",
                    "9",
                    "https://home.invalid/@alice/10",
                    TimeDuration::minutes(5)
                ),
                // not a reply at all
                json!({
                    "id": "11",
                    "url": "https://home.invalid/@alice/11",
                    "uri": "https://home.invalid/@alice/11",
                    "created_at": timeutil::fmt_state_ts(timeutil::now()),
                    "visibility": "public",
                }),
                // a reply, but one we have already seen
                reply_status(
                    "7",
                    "6",
                    "https://home.invalid/@alice/7",
                    TimeDuration::minutes(5)
                ),
                // a reply, but older than the interval
                reply_status(
                    "8",
                    "6",
                    "https://home.invalid/@alice/8",
                    TimeDuration::hours(2)
                ),
            ]));
        })
        .await;

    let toots = reply_toots(&mut ctx, &["42".to_string()]).await;

    assert_eq!(toots.len(), 1, "only the fresh unseen reply counts");
    assert_eq!(toots[0].url, "https://home.invalid/@alice/10");
}

/// One user's statuses failing does not stop the others being read.
#[tokio::test]
async fn reply_toots_survives_a_failing_user() {
    let server = MockServer::start_async().await;
    let dir = common::tmp_dir();
    let mut config = common::test_config(&dir, "home.invalid");
    config.reply_interval_in_hours = 1;
    let db = db::open(&config).await.unwrap();
    let http = HttpClient::new(config.clone(), db.clone());
    let home = HomeServer::with_scheme(&host(&server), "http", "token", http.clone());
    let mut state = State::default();
    let mut ctx = common::run_context(&config, &mut state, &http, &home, &db);

    server
        .mock_async(|when, then| {
            when.method(Method::GET)
                .path("/api/v1/accounts/bad/statuses");
            then.status(500);
        })
        .await;
    server
        .mock_async(|when, then| {
            when.method(Method::GET)
                .path("/api/v1/accounts/good/statuses");
            then.status(200).json_body(json!([reply_status(
                "10",
                "9",
                "https://home.invalid/@good/10",
                TimeDuration::minutes(5),
            )]));
        })
        .await;

    let logs = common::LogCapture::new();
    let guard = logs.install();
    let toots = reply_toots(&mut ctx, &["bad".to_string(), "good".to_string()]).await;
    drop(guard);

    assert_eq!(toots.len(), 1, "the readable user's replies are kept");
    let logged = logs.text();
    assert!(
        logged.contains("Error getting replies for user bad"),
        "the failure is logged: {logged}"
    );
}

/// A timeline post's author and mentions are backfilled when the setting
/// asks for it — reaching for their server, which here is not there.
#[tokio::test]
async fn a_timeline_posts_mentions_are_backfilled() {
    let server = MockServer::start_async().await;
    let dir = common::tmp_dir();
    let mut config = common::test_config(&dir, "home.invalid");
    config.home_timeline_length = 5;
    let db = db::open(&config).await.unwrap();
    let http = HttpClient::new(config.clone(), db.clone());
    let home = HomeServer::with_scheme(&host(&server), "http", "token", http.clone());
    let mut state = State::default();
    let mut ctx = common::run_context(&config, &mut state, &http, &home, &db);

    server
        .mock_async(|when, then| {
            when.method(Method::GET).path("/api/v1/timelines/home");
            then.status(200).json_body(json!([
                // a post by a remote author, mentioning another remote
                // account, boosting a third
                json!({
                    "id": "1",
                    "url": "https://home.invalid/@me/1",
                    "uri": "https://home.invalid/@me/1",
                    "created_at": timeutil::fmt_state_ts(timeutil::now()),
                    "visibility": "public",
                    "account": account("alice", "https://unreach.invalid/@alice"),
                    "mentions": [account("bob", "https://unreach.invalid/@bob")],
                    "reblog": {
                        "id": "2",
                        "url": "https://unreach.invalid/@carol/2",
                        "uri": "https://unreach.invalid/@carol/2",
                        "created_at": timeutil::fmt_state_ts(timeutil::now()),
                        "visibility": "public",
                        "account": account("carol", "https://unreach.invalid/@carol"),
                    },
                })
            ]));
        })
        .await;

    let logs = common::LogCapture::new();
    let guard = logs.install();
    let posts = ctx.home.timeline(5).await.unwrap();
    fetch_timeline_context(&mut ctx, posts, "home timeline").await;
    drop(guard);

    let logged = logs.text();
    assert!(
        logged.contains("server unreach.invalid not found for post"),
        "each mentioned account's server is reached for: {logged}"
    );
    assert!(
        logged
            .matches("server unreach.invalid not found for post")
            .count()
            >= 3,
        "the author, the mention and the boosted account are each tried: {logged}"
    );
}
