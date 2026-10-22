//! Page-at-a-time fetching against a real server: the `Link` header paging
//! the home server's endpoints use.

mod common;

use fedifetcher::api::mastodon::{StopAt, get_paginated};
use fedifetcher::http::HttpClient;
use httpmock::prelude::*;
use httpmock::Method;
use serde_json::json;

fn client(dir: &std::path::Path) -> HttpClient {
    // the paging code is not about robots.txt; leave it out of the way
    HttpClient::new(common::test_config(dir, "example.social")).ignoring_robots()
}

#[tokio::test]
async fn follows_link_headers_until_the_server_runs_out() {
    let server = MockServer::start_async().await;
    let dir = common::tmp_dir();
    // the first page's mock must not match the second request, or the same
    // Link header would be followed forever
    let page1 = server
        .mock_async(|when, then| {
            when.method(Method::GET)
                .path("/api/v1/timelines/home")
                .matches(|req| {
                    req.query_params
                        .as_ref()
                        .map(|params| params.iter().all(|(key, _)| key != "max_id"))
                        .unwrap_or(true)
                });
            then.status(200)
                .header(
                    "Link",
                    format!(
                        r#"<{}/api/v1/timelines/home?max_id=2>; rel="next""#,
                        server.base_url()
                    ),
                )
                .json_body(json!([{"id": "2"}, {"id": "1"}]));
        })
        .await;
    let page2 = server
        .mock_async(|when, then| {
            when.method(Method::GET)
                .path("/api/v1/timelines/home")
                .query_param("max_id", "2");
            then.status(200).json_body(json!([{"id": "0"}]));
        })
        .await;
    let http = client(&dir);

    let items = get_paginated(
        &format!("{}/api/v1/timelines/home", server.base_url()),
        &StopAt::All,
        None,
        5,
        &http,
        "read:statuses",
        None,
    )
    .await
    .expect("paging should succeed");

    assert_eq!(items.len(), 3);
    assert_eq!(page1.hits(), 1);
    assert_eq!(page2.hits(), 1);
}

#[tokio::test]
async fn stops_once_it_has_the_count_it_wanted() {
    let server = MockServer::start_async().await;
    let dir = common::tmp_dir();
    let page1 = server
        .mock_async(|when, then| {
            when.method(Method::GET)
                .path("/api/v1/timelines/home")
                .query_param("limit", "3");
            then.status(200)
                .header(
                    "Link",
                    format!(
                        r#"<{}/api/v1/timelines/home?max_id=1>; rel="next""#,
                        server.base_url()
                    ),
                )
                .json_body(json!([{"id": "2"}, {"id": "1"}]));
        })
        .await;
    let page2 = server
        .mock_async(|when, then| {
            when.method(Method::GET)
                .path("/api/v1/timelines/home")
                .query_param("max_id", "1");
            then.status(200).json_body(json!([{"id": "0"}, {"id": "-1"}]));
        })
        .await;
    let http = client(&dir);

    let items = get_paginated(
        &format!("{}/api/v1/timelines/home", server.base_url()),
        &StopAt::Count(3),
        None,
        5,
        &http,
        "read:statuses",
        None,
    )
    .await
    .expect("paging should succeed");

    // pages arrive whole: the second page is read to its end before stopping
    assert_eq!(items.len(), 4);
    assert_eq!(page1.hits(), 1);
    assert_eq!(page2.hits(), 1);
}

#[tokio::test]
async fn an_empty_page_ends_the_search() {
    let server = MockServer::start_async().await;
    let dir = common::tmp_dir();
    server
        .mock_async(|when, then| {
            when.method(Method::GET)
                .path("/api/v1/timelines/home")
                .matches(|req| {
                    req.query_params
                        .as_ref()
                        .map(|params| params.iter().all(|(key, _)| key != "max_id"))
                        .unwrap_or(true)
                });
            then.status(200)
                .header(
                    "Link",
                    format!(
                        r#"<{}/api/v1/timelines/home?max_id=2>; rel="next""#,
                        server.base_url()
                    ),
                )
                .json_body(json!([{"id": "2"}, {"id": "1"}]));
        })
        .await;
    let page2 = server
        .mock_async(|when, then| {
            when.method(Method::GET)
                .path("/api/v1/timelines/home")
                .query_param("max_id", "2");
            then.status(200).json_body(json!([]));
        })
        .await;
    let http = client(&dir);

    let items = get_paginated(
        &format!("{}/api/v1/timelines/home", server.base_url()),
        &StopAt::All,
        None,
        5,
        &http,
        "read:statuses",
        None,
    )
    .await
    .expect("paging should succeed");

    assert_eq!(items.len(), 2);
    assert_eq!(page2.hits(), 1);
}

#[tokio::test]
async fn stops_when_there_is_no_next_link() {
    let server = MockServer::start_async().await;
    let dir = common::tmp_dir();
    let page1 = server
        .mock_async(|when, then| {
            when.method(Method::GET).path("/api/v1/timelines/home");
            then.status(200).json_body(json!([{"id": "2"}, {"id": "1"}]));
        })
        .await;
    let page2 = server
        .mock_async(|when, then| {
            when.method(Method::GET)
                .path("/api/v1/timelines/home")
                .query_param_exists("max_id");
            then.status(200).json_body(json!([{"id": "0"}]));
        })
        .await;
    let http = client(&dir);

    let items = get_paginated(
        &format!("{}/api/v1/timelines/home", server.base_url()),
        &StopAt::All,
        None,
        5,
        &http,
        "read:statuses",
        None,
    )
    .await
    .expect("paging should succeed");

    assert_eq!(items.len(), 2);
    assert_eq!(page1.hits(), 1);
    assert_eq!(page2.hits(), 0, "without a next link there is no page 2");
}

#[tokio::test]
async fn an_error_status_is_reported() {
    let server = MockServer::start_async().await;
    let dir = common::tmp_dir();
    server
        .mock_async(|when, then| {
            when.method(Method::GET).path("/api/v1/timelines/home");
            then.status(500);
        })
        .await;
    let http = client(&dir);

    let err = get_paginated(
        &format!("{}/api/v1/timelines/home", server.base_url()),
        &StopAt::All,
        None,
        5,
        &http,
        "read:statuses",
        None,
    )
    .await
    .err()
    .expect("a 500 must be an error");
    assert!(err.to_string().contains("500"), "{err}");
}
