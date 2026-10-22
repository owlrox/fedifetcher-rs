//! Backfilling one account: the opt-out rules, and that skipping an account
//! is logged with the reason.

mod common;

use fedifetcher::backfill::{UserSet, get_user_posts};
use fedifetcher::http::HttpClient;
use fedifetcher::store::State;
use fedifetcher::users::User;
use httpmock::Method;
use httpmock::prelude::*;

#[tokio::test]
async fn an_account_that_opted_out_is_logged_and_not_fetched() {
    let server = MockServer::start_async().await;
    let dir = common::tmp_dir();
    let config = common::test_config(&dir, "example.social");
    let client = HttpClient::new(config.clone());
    let mut state = State::default();

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
    let posts = get_user_posts(
        &user,
        UserSet::RecentlyCheckedUsers,
        "example.social",
        &client,
        &config,
        &mut state,
    )
    .await;
    drop(guard);

    assert!(posts.is_none(), "an opted-out account is skipped");
    assert_eq!(nodeinfo.hits(), 0, "nothing is fetched for the account");
    assert_eq!(robots.hits(), 0, "nothing is fetched for the account");
    let logged = logs.text();
    let line = logged
        .lines()
        .find(|line| line.contains("opted out"))
        .unwrap_or_else(|| panic!("the skip should be logged, got:\n{logged}"));
    assert!(line.contains("bot"), "the log line names the account: {line}");
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
    let client = HttpClient::new(config.clone());
    let mut state = State::default();

    // A local user: no fetching happens either way, but with the rule off
    // the opt-out check is skipped and the account is recorded as checked.
    let mut user = User::new("bot", "https://example.social/@bot");
    user.note = "beep boop, nobot".to_string();

    let logs = common::LogCapture::new();
    let guard = logs.install();
    let posts = get_user_posts(
        &user,
        UserSet::RecentlyCheckedUsers,
        "example.social",
        &client,
        &config,
        &mut state,
    )
    .await;
    drop(guard);

    assert!(posts.is_none(), "a local user has no posts to fetch");
    assert!(
        !logs.text().contains("opted out"),
        "no opt-out is logged when the rule is off: {}",
        logs.text()
    );
    assert!(
        state.recently_checked_users.contains("bot"),
        "the account is recorded as checked"
    );
}
