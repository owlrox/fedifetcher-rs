//! The debug tab's actions against a mock server: the connection test, a
//! user lookup, and a post fetch. The actions take the server's base URL,
//! so these tests point them straight at the mock.

mod common;

use fedifetcher::debug::{account_posts, lookup_account, test_connection, verify_credentials};
use fedifetcher::http::HttpClient;
use httpmock::prelude::*;
use httpmock::Method;
use serde_json::json;

fn client(dir: &std::path::Path) -> HttpClient {
    HttpClient::new(common::test_config(dir, "example.social"))
}

#[tokio::test]
async fn the_connection_test_reports_instance_and_account() {
    let server = MockServer::start_async().await;
    let instance = server
        .mock_async(|when, then| {
            when.method(Method::GET).path("/api/v1/instance");
            then.status(200).json_body(
                json!({"title": "Example", "uri": "example.social", "version": "4.2.0"}),
            );
        })
        .await;
    let verify = server
        .mock_async(|when, then| {
            when.method(Method::GET)
                .path("/api/v1/accounts/verify_credentials")
                .header("Authorization", "Bearer sekret-token");
            then.status(200)
                .json_body(json!({"id": "1", "acct": "me", "display_name": "Me"}));
        })
        .await;
    let dir = common::tmp_dir();
    let http = client(&dir);

    let (info, account) = test_connection(&server.base_url(), "sekret-token", &http)
        .await
        .unwrap();

    assert_eq!(info["version"], "4.2.0");
    assert_eq!(account["acct"], "me");
    assert_eq!(instance.hits(), 1);
    assert_eq!(verify.hits(), 1, "the token goes as a Bearer header");
}

#[tokio::test]
async fn a_rejected_token_is_reported_with_a_masked_hint() {
    let server = MockServer::start_async().await;
    server
        .mock_async(|when, then| {
            when.method(Method::GET)
                .path("/api/v1/accounts/verify_credentials");
            then.status(401)
                .json_body(json!({"error": "The access token is invalid"}));
        })
        .await;
    let dir = common::tmp_dir();
    let http = client(&dir);

    let err = verify_credentials(&server.base_url(), "sekret-token", &http)
        .await
        .unwrap_err();

    let message = err.to_string();
    assert!(message.contains("Status code: 401"), "{message}");
    assert!(
        message.contains("access token is incorrect"),
        "{message}"
    );
    assert!(message.contains("sekre"), "the masked hint: {message}");
    assert!(
        !message.contains("sekret-token"),
        "the full token must not appear: {message}"
    );
}

#[tokio::test]
async fn a_user_lookup_returns_the_account() {
    let server = MockServer::start_async().await;
    let lookup = server
        .mock_async(|when, then| {
            when.method(Method::GET)
                .path("/api/v1/accounts/lookup")
                .query_param("acct", "someone@example.social")
                .header("Authorization", "Bearer sekret-token");
            then.status(200).json_body(
                json!({"id": "42", "acct": "someone@example.social", "display_name": "Someone"}),
            );
        })
        .await;
    let dir = common::tmp_dir();
    let http = client(&dir);

    let account =
        lookup_account(&server.base_url(), "@someone@example.social", "sekret-token", &http)
            .await
            .unwrap();

    assert_eq!(account["id"], "42");
    assert_eq!(account["display_name"], "Someone");
    assert_eq!(lookup.hits(), 1, "a leading @ is trimmed before the lookup");
}

#[tokio::test]
async fn a_missing_user_is_reported() {
    let server = MockServer::start_async().await;
    server
        .mock_async(|when, then| {
            when.method(Method::GET).path("/api/v1/accounts/lookup");
            then.status(404);
        })
        .await;
    let dir = common::tmp_dir();
    let http = client(&dir);

    let err = lookup_account(&server.base_url(), "ghost", "sekret-token", &http)
        .await
        .unwrap_err();

    assert!(err.to_string().contains("ghost was not found"), "{err}");
}

#[tokio::test]
async fn posts_are_fetched_for_the_looked_up_user() {
    let server = MockServer::start_async().await;
    server
        .mock_async(|when, then| {
            when.method(Method::GET)
                .path("/api/v1/accounts/lookup")
                .query_param("acct", "someone");
            then.status(200)
                .json_body(json!({"id": "42", "acct": "someone"}));
        })
        .await;
    let statuses = server
        .mock_async(|when, then| {
            when.method(Method::GET)
                .path("/api/v1/accounts/42/statuses")
                .query_param("exclude_replies", "false")
                .query_param("limit", "2");
            then.status(200).json_body(json!([
                {"id": "9", "content": "<p>second</p>", "created_at": "2024-01-02T00:00:00.000Z"},
                {"id": "8", "content": "<p>first</p>", "created_at": "2024-01-01T00:00:00.000Z"}
            ]));
        })
        .await;
    let dir = common::tmp_dir();
    let http = client(&dir);

    let (account, posts) = account_posts(&server.base_url(), "someone", 2, "sekret-token", &http)
        .await
        .unwrap();

    assert_eq!(account["acct"], "someone");
    assert_eq!(posts.len(), 2);
    assert_eq!(posts[0]["id"], "9");
    assert_eq!(statuses.hits(), 1, "the limit goes to the server");
}

#[tokio::test]
async fn a_failed_lookup_stops_a_post_fetch() {
    let server = MockServer::start_async().await;
    server
        .mock_async(|when, then| {
            when.method(Method::GET).path("/api/v1/accounts/lookup");
            then.status(404);
        })
        .await;
    let dir = common::tmp_dir();
    let http = client(&dir);

    let err = account_posts(&server.base_url(), "ghost", 5, "sekret-token", &http)
        .await
        .unwrap_err();

    assert!(err.to_string().contains("ghost was not found"), "{err}");
}
