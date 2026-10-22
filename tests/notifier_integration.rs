//! The monitoring callbacks, sent through a real client so that a mock
//! would not accept any URL we happened to build.

mod common;

use fedifetcher::http::HttpClient;
use fedifetcher::run::Notifier;
use httpmock::prelude::*;
use httpmock::Method;
use uuid::Uuid;

fn notifier(dir: &std::path::Path, on_start: Option<String>, on_done: Option<String>, on_fail: Option<String>) -> Notifier {
    let mut config = common::test_config(dir, "example.social");
    config.on_start = on_start;
    config.on_done = on_done;
    config.on_fail = on_fail;
    let http = HttpClient::new(common::test_config(dir, "example.social"));
    Notifier::new(config, http, Uuid::from_u128(1))
}

#[tokio::test]
async fn the_callback_is_really_sent() {
    let server = MockServer::start_async().await;
    let dir = common::tmp_dir();
    let robots = server
        .mock_async(|when, then| {
            when.method(Method::GET).path("/robots.txt");
            then.status(200);
        })
        .await;
    let ping = server
        .mock_async(|when, then| {
            when.method(Method::GET).path("/start").query_param_exists("rid");
            then.status(200);
        })
        .await;
    let notify = notifier(
        &dir,
        Some(format!("{}/start", server.base_url())),
        None,
        None,
    );

    notify.starting().await;

    assert_eq!(ping.hits(), 1);
    assert_eq!(robots.hits(), 0, "callbacks ignore robots.txt");
}

#[tokio::test]
async fn callbacks_carry_the_run_id() {
    let server = MockServer::start_async().await;
    let dir = common::tmp_dir();
    let ping = server
        .mock_async(|when, then| {
            when.method(Method::GET).path("/start").query_param(
                "rid",
                "00000000-0000-0000-0000-000000000001",
            );
            then.status(200);
        })
        .await;
    let notify = notifier(
        &dir,
        Some(format!("{}/start", server.base_url())),
        None,
        None,
    );

    notify.starting().await;

    assert_eq!(ping.hits(), 1);
}

#[tokio::test]
async fn the_done_callback_reports_how_long_it_took() {
    let server = MockServer::start_async().await;
    let dir = common::tmp_dir();
    let ping = server
        .mock_async(|when, then| {
            when.method(Method::GET)
                .path("/done")
                .query_param_exists("rid")
                .query_param_exists("ping")
                .query_param_exists("msg");
            then.status(200);
        })
        .await;
    let notify = notifier(&dir, None, Some(format!("{}/done", server.base_url())), None);

    notify.done("all finished").await;

    assert_eq!(ping.hits(), 1);
}

#[tokio::test]
async fn unset_callbacks_are_not_called() {
    let server = MockServer::start_async().await;
    let dir = common::tmp_dir();
    let any = server
        .mock_async(|when, then| {
            when.method(Method::GET);
            then.status(200);
        })
        .await;
    let notify = notifier(&dir, None, None, None);

    notify.starting().await;
    notify.done("finished").await;
    notify.failed("broke").await;

    assert_eq!(any.hits(), 0);
}

#[tokio::test]
async fn a_failing_callback_does_not_stop_the_run() {
    let dir = common::tmp_dir();
    // port 9 (discard) is closed: the callback goes nowhere
    let notify = notifier(
        &dir,
        None,
        Some("http://127.0.0.1:9/done".to_string()),
        Some("http://127.0.0.1:9/fail".to_string()),
    );

    notify.done("all finished").await;
    notify.failed("broke").await;
}
