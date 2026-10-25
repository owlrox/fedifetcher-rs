//! Every fediverse software's API client against a real server over http,
//! end to end: URL building, authentication, paging and response parsing.
//! httpmock only speaks http, which is why the clients can be pointed at it
//! with `with_scheme`; production always uses `new` (https).

mod common;
mod fixture;

use fedifetcher::api::Client;
use fedifetcher::api::lemmy::LemmyApi;
use fedifetcher::api::mastodon::{HomeServer, MastodonApi};
use fedifetcher::api::misskey::MisskeyApi;
use fedifetcher::api::peertube::PeerTubeApi;
use fedifetcher::http::HttpClient;
use httpmock::prelude::*;
use httpmock::{Method, Then, When};

const ALLOW_ALL: &str = "User-agent: *\nAllow: /\n";

/// The mock server's host:port, as the clients want it.
fn host(server: &MockServer) -> String {
    server
        .base_url()
        .strip_prefix("http://")
        .unwrap()
        .to_string()
}

async fn http(dir: &std::path::Path) -> HttpClient {
    let config = common::test_config(dir, "home.example");
    let db = fixture::test_db(&config).await;
    HttpClient::new(config, db)
}

/// Serve an allow-everything robots.txt, so a test can get on with the part
/// it is actually about.
async fn allow_all_robots(server: &MockServer) {
    server
        .mock_async(|when, then| {
            when.method(Method::GET).path("/robots.txt");
            then.status(200).body(ALLOW_ALL);
        })
        .await;
}

#[tokio::test]
async fn the_mastodon_client_runs_against_a_live_http_server() {
    let server = MockServer::start_async().await;
    let dir = common::tmp_dir();
    let host = host(&server);
    allow_all_robots(&server).await;
    let api = Client::Mastodon(MastodonApi::with_scheme(&host, "http", http(&dir).await));

    server
        .mock_async(|when, then| {
            when.method(Method::GET)
                .path("/api/v1/accounts/lookup")
                .query_param("acct", "alice");
            then.status(200).json_body(serde_json::json!({ "id": "1" }));
        })
        .await;
    server
        .mock_async(|when, then| {
            when.method(Method::GET).path("/api/v1/accounts/1/statuses");
            then.status(200).json_body(serde_json::json!([
                {
                    "id": "10",
                    "url": format!("http://{host}/@alice/10"),
                    "uri": format!("http://{host}/@alice/10"),
                    "created_at": "2026-10-22T10:00:00.000Z",
                    "visibility": "public",
                    "reblog": null,
                },
                {
                    "id": "11",
                    "created_at": "2026-10-22T11:00:00.000Z",
                    "visibility": "public",
                    "reblog": {
                        "id": "12",
                        "url": format!("http://{host}/@bob/12"),
                        "uri": format!("http://{host}/@bob/12"),
                        "created_at": "2026-10-22T11:00:00.000Z",
                        "visibility": "public",
                        "reblog": null,
                    },
                },
            ]));
        })
        .await;

    let posts = api
        .fetch_user_posts("alice", &format!("http://{host}/@alice"), 10)
        .await
        .expect("the user's posts");
    assert_eq!(posts.len(), 2);
    assert_eq!(posts[0].url, format!("http://{host}/@alice/10"));
    assert!(posts[0].is_public);
    assert_eq!(
        posts[1].url,
        format!("http://{host}/@bob/12"),
        "a boost surfaces the boosted post's url"
    );
    assert!(posts[1].is_boost());

    server
        .mock_async(|when, then| {
            when.method(Method::GET).path("/api/v1/statuses/10/context");
            then.status(200).json_body(serde_json::json!({
                "ancestors": [{ "url": format!("http://{host}/@alice/9") }],
                "descendants": [{ "url": format!("http://{host}/@alice/11") }],
            }));
        })
        .await;
    let urls = api
        .fetch_context_urls("10", &format!("http://{host}/@alice/10"))
        .await;
    assert_eq!(
        urls,
        vec![
            format!("http://{host}/@alice/9"),
            format!("http://{host}/@alice/11")
        ]
    );

    // a Pleroma/Mitra object URL, whose id only the redirect reveals
    server
        .mock_async(|when, then| {
            when.method(Method::HEAD).path("/objects/xyz");
            then.status(302)
                .header("Location", format!("http://{host}/post/abc123"));
        })
        .await;
    assert_eq!(
        api.post_id_from(&format!("http://{host}/objects/xyz"))
            .await
            .as_deref(),
        Some("abc123")
    );

    assert_eq!(
        api.post_id_from(&format!("http://{host}/@alice/77"))
            .await
            .as_deref(),
        Some("77")
    );
    assert_eq!(
        api.username_from(&format!("http://{host}/@alice")),
        Some("alice".to_string())
    );
}

#[tokio::test]
async fn the_misskey_client_runs_against_a_live_http_server() {
    let server = MockServer::start_async().await;
    let dir = common::tmp_dir();
    let host = host(&server);
    allow_all_robots(&server).await;
    let api = Client::Misskey(MisskeyApi::with_scheme(&host, "http", http(&dir).await));

    server
        .mock_async(|when, then| {
            when.method(Method::POST)
                .path("/api/users/search-by-username-and-host");
            then.status(200)
                .json_body(serde_json::json!([{ "id": "u1", "host": null }]));
        })
        .await;
    server
        .mock_async(|when, then| {
            when.method(Method::POST).path("/api/users/notes");
            then.status(200).json_body(serde_json::json!([
                {
                    "id": "n1",
                    "createdAt": "2026-10-22T10:00:00.000Z",
                    "visibility": "public",
                    "url": format!("http://{host}/notes/n1"),
                },
                {
                    "id": "n2",
                    "createdAt": "2026-10-22T11:00:00.000Z",
                    "visibility": "home",
                },
            ]));
        })
        .await;

    let posts = api
        .fetch_user_posts("alice", &format!("http://{host}/@alice"), 10)
        .await
        .expect("the user's notes");
    assert_eq!(posts.len(), 2);
    assert_eq!(posts[0].url, format!("http://{host}/notes/n1"));
    assert_eq!(
        posts[1].url,
        format!("http://{host}/notes/n2"),
        "a note without a url gets one from the server's base"
    );
    assert!(posts[1].is_public, "home is public-but-unlisted");

    server
        .mock_async(|when, then| {
            when.method(Method::POST).path("/api/notes/children");
            then.status(200)
                .json_body(serde_json::json!([{ "id": "c1" }]));
        })
        .await;
    server
        .mock_async(|when, then| {
            when.method(Method::POST).path("/api/notes/conversation");
            then.status(200)
                .json_body(serde_json::json!([{ "id": "c2" }]));
        })
        .await;
    let urls = api
        .fetch_context_urls("n1", &format!("http://{host}/notes/n1"))
        .await;
    assert_eq!(
        urls,
        vec![
            format!("http://{host}/notes/c1"),
            format!("http://{host}/notes/c2")
        ]
    );

    assert_eq!(
        api.post_id_from(&format!("http://{host}/notes/n1")).await,
        Some("n1".to_string())
    );
    assert_eq!(
        api.username_from(&format!("http://{host}/@alice")),
        Some("alice".to_string())
    );
}

#[tokio::test]
async fn the_lemmy_client_runs_against_a_live_http_server() {
    let server = MockServer::start_async().await;
    let dir = common::tmp_dir();
    let host = host(&server);
    allow_all_robots(&server).await;
    let api = Client::Lemmy(LemmyApi::with_scheme(&host, "http", http(&dir).await));

    server
        .mock_async(|when, then| {
            when.method(Method::GET)
                .path("/api/v3/user")
                .query_param("username", "alice");
            then.status(200).json_body(serde_json::json!({
                "comments": [],
                "posts": [{
                    "post": {
                        "ap_id": format!("http://{host}/post/1"),
                        "published": "2026-10-22T10:00:00Z",
                    },
                }],
            }));
        })
        .await;

    let posts = api
        .fetch_user_posts("alice", &format!("http://{host}/u/alice"), 10)
        .await
        .expect("the user's posts");
    assert_eq!(posts.len(), 1);
    assert_eq!(posts[0].url, format!("http://{host}/post/1"));

    server
        .mock_async(|when, then| {
            when.method(Method::GET)
                .path("/api/v3/post")
                .query_param("id", "5");
            then.status(200).json_body(serde_json::json!({
                "post_view": {
                    "counts": { "comments": 1 },
                    "post": { "ap_id": format!("http://{host}/post/5") },
                },
            }));
        })
        .await;
    server
        .mock_async(|when, then| {
            when.method(Method::GET)
                .path("/api/v3/comment/list")
                .query_param("post_id", "5");
            then.status(200).json_body(serde_json::json!({
                "comments": [{ "comment": { "ap_id": format!("http://{host}/comment/7") } }],
            }));
        })
        .await;
    let urls = api
        .fetch_context_urls("5", &format!("http://{host}/post/5"))
        .await;
    assert_eq!(
        urls,
        vec![
            format!("http://{host}/post/5"),
            format!("http://{host}/comment/7")
        ]
    );

    assert_eq!(
        api.post_id_from(&format!("http://{host}/post/123")).await,
        Some("123".to_string())
    );
    assert_eq!(
        api.username_from(&format!("http://{host}/u/alice")),
        Some("alice".to_string())
    );
}

#[tokio::test]
async fn the_peertube_client_runs_against_a_live_http_server() {
    let server = MockServer::start_async().await;
    let dir = common::tmp_dir();
    let host = host(&server);
    allow_all_robots(&server).await;
    let api = Client::PeerTube(PeerTubeApi::with_scheme(&host, "http", http(&dir).await));

    server
        .mock_async(|when, then| {
            when.method(Method::GET)
                .path("/api/v1/accounts/alice/videos");
            then.status(200).json_body(serde_json::json!({
                "data": [{
                    "uuid": "v1",
                    "url": format!("http://{host}/videos/watch/v1"),
                    "publishedAt": "2026-10-22T10:00:00Z",
                    "privacy": { "id": 1 },
                }],
            }));
        })
        .await;

    let posts = api
        .fetch_user_posts("alice", &format!("http://{host}/accounts/alice"), 10)
        .await
        .expect("the account's videos");
    assert_eq!(posts.len(), 1);
    assert_eq!(posts[0].url, format!("http://{host}/videos/watch/v1"));
    assert!(posts[0].is_public);

    server
        .mock_async(|when, then| {
            when.method(Method::GET)
                .path("/api/v1/videos/v1/comment-threads");
            then.status(200).json_body(serde_json::json!({
                "data": [{ "url": format!("http://{host}/videos/watch/v1/comments/1") }],
            }));
        })
        .await;
    let urls = api
        .fetch_context_urls("v1", &format!("http://{host}/videos/watch/v1"))
        .await;
    assert_eq!(
        urls,
        vec![format!("http://{host}/videos/watch/v1/comments/1")]
    );

    assert_eq!(
        api.post_id_from(&format!("http://{host}/w/abc")).await,
        Some("abc".to_string())
    );
    assert_eq!(
        api.username_from(&format!("http://{host}/accounts/alice")),
        Some("alice".to_string())
    );
}

#[tokio::test]
async fn the_home_server_client_runs_against_a_live_http_server() {
    let server = MockServer::start_async().await;
    let dir = common::tmp_dir();
    let host = host(&server);
    // the home server client is authenticated, so it ignores robots.txt
    let home = HomeServer::with_scheme(&host, "http", "secret-token", http(&dir).await);

    let status = |id: &str, user: &str, hour: &str| {
        serde_json::json!({
            "id": id,
            "url": format!("http://{host}/@{user}/{id}"),
            "uri": format!("http://{host}/@{user}/{id}"),
            "created_at": format!("2026-10-22T{hour}:00:00.000Z"),
            "visibility": "public",
            "reblog": null,
        })
    };
    server
        .mock_async(|when, then| {
            when.method(Method::GET)
                .path("/api/v1/timelines/home")
                .query_param("limit", "10")
                .header("Authorization", "Bearer secret-token");
            then.status(200)
                .json_body(serde_json::json!([status("10", "alice", "10")]))
                .header(
                    "Link",
                    format!(
                        "<{}/api/v1/timelines/home?max_id=10>; rel=\"next\"",
                        server.base_url()
                    ),
                );
        })
        .await;
    server
        .mock_async(|when, then| {
            when.method(Method::GET)
                .path("/api/v1/timelines/home")
                .query_param("max_id", "10")
                .header("Authorization", "Bearer secret-token");
            then.status(200)
                .json_body(serde_json::json!([status("9", "bob", "09")]));
        })
        .await;

    let posts = home.timeline(10).await.expect("the home timeline");
    assert_eq!(posts.len(), 2, "both pages are fetched");
    assert_eq!(posts[0].url, format!("http://{host}/@alice/10"));
    assert_eq!(posts[1].url, format!("http://{host}/@bob/9"));

    server
        .mock_async(|when, then| {
            when.method(Method::GET)
                .path("/api/v1/accounts/verify_credentials")
                .header("Authorization", "Bearer secret-token");
            then.status(200)
                .json_body(serde_json::json!({ "id": "42" }));
        })
        .await;
    assert_eq!(home.user_id(None).await.unwrap(), "42");
}

/// A 403 names the permission the access token is missing, so the log says
/// which scope to add — for every endpoint a run reads.
#[tokio::test]
async fn a_403_names_the_missing_permission() {
    let server = MockServer::start_async().await;
    let dir = common::tmp_dir();
    let host = host(&server);
    // the home server client is authenticated, so it ignores robots.txt
    let home = HomeServer::with_scheme(&host, "http", "secret-token", http(&dir).await);

    for path in [
        "/api/v1/bookmarks",
        "/api/v1/favourites",
        "/api/v1/follow_requests",
        "/api/v1/lists",
        "/api/v1/notifications",
    ] {
        server
            .mock_async(|when, then| {
                when.method(Method::GET).path(path);
                then.status(403);
            })
            .await;
    }

    let err = home.bookmarks(10).await.unwrap_err().to_string();
    assert!(err.contains("Status code: 403"), "{err}");
    assert!(err.contains("read:bookmarks"), "the scope is named: {err}");

    let err = home.favorites(10).await.unwrap_err().to_string();
    assert!(err.contains("read:favorites"), "the scope is named: {err}");

    let err = home.follow_requests(10).await.unwrap_err().to_string();
    assert!(err.contains("read:follows"), "the scope is named: {err}");

    let err = home.lists().await.unwrap_err().to_string();
    assert!(err.contains("read:lists"), "the scope is named: {err}");

    let err = home
        .notification_accounts(fedifetcher::timeutil::now())
        .await
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("read:notifications"),
        "the scope is named: {err}"
    );
}

/// A community listing is fetched through its own collection.
#[tokio::test]
async fn the_lemmy_client_reads_a_community_listing() {
    let server = MockServer::start_async().await;
    let dir = common::tmp_dir();
    let host = host(&server);
    allow_all_robots(&server).await;
    let api = Client::Lemmy(LemmyApi::with_scheme(&host, "http", http(&dir).await));

    let mock_host = host.clone();
    server
        .mock_async(move |when, then| {
            when.method(Method::GET)
                .path("/api/v3/post/list")
                .query_param("community_name", "asklemmy");
            then.status(200).json_body(serde_json::json!({
                "posts": [{
                    "post": {
                        "ap_id": format!("http://{mock_host}/post/9"),
                        "published": "2026-10-22T10:00:00Z",
                    },
                }],
            }));
        })
        .await;

    let posts = api
        .fetch_user_posts("asklemmy", &format!("https://{host}/c/asklemmy"), 10)
        .await
        .expect("the community's posts");
    assert_eq!(posts.len(), 1);
    assert_eq!(posts[0].url, format!("http://{host}/post/9"));
}

/// A profile URL of no known shape is reported and yields nothing.
#[tokio::test]
async fn a_lemmy_profile_url_of_no_known_shape_yields_nothing() {
    let server = MockServer::start_async().await;
    let dir = common::tmp_dir();
    let host = host(&server);
    allow_all_robots(&server).await;
    let api = Client::Lemmy(LemmyApi::with_scheme(&host, "http", http(&dir).await));

    let posts = api
        .fetch_user_posts("alice", &format!("https://{host}/weird/alice"), 10)
        .await;
    assert!(posts.is_none(), "an unknown shape fetches nothing");
}

/// A paged collection fetch reports each way it can fail, and yields
/// nothing on every one of them.
#[tokio::test]
async fn a_paged_lemmy_fetch_reports_each_way_it_can_fail() {
    let server = MockServer::start_async().await;
    let dir = common::tmp_dir();
    let host = host(&server);
    allow_all_robots(&server).await;
    let api = Client::Lemmy(LemmyApi::with_scheme(&host, "http", http(&dir).await));
    let profile = format!("https://{host}/u/alice");

    server
        .mock_async(|when, then| {
            when.method(Method::GET).path("/api/v3/user");
            then.status(500);
        })
        .await;
    let posts = api.fetch_user_posts("alice", &profile, 10).await;
    assert!(posts.is_none(), "a failed page yields nothing");

    server
        .mock_async(|when, then| {
            when.method(Method::GET).path("/api/v3/user");
            then.status(200).body("not json");
        })
        .await;
    let posts = api.fetch_user_posts("alice", &profile, 10).await;
    assert!(posts.is_none(), "an unreadable page yields nothing");

    server
        .mock_async(|when, then| {
            when.method(Method::GET).path("/api/v3/user");
            then.status(200).json_body(serde_json::json!({
                "comments": [{}],
                "posts": [],
            }));
        })
        .await;
    let posts = api.fetch_user_posts("alice", &profile, 10).await;
    assert!(
        posts.is_none(),
        "a page whose entries have no posts yields nothing"
    );
}

/// A comment's context resolves through the post the comment is on: the
/// comment names the post, and the post's own comments are the context. The
/// post id may arrive as a number or a string.
#[tokio::test]
async fn a_lemmy_comments_context_is_read_through_its_post() {
    let server = MockServer::start_async().await;
    let dir = common::tmp_dir();
    let host = host(&server);
    allow_all_robots(&server).await;
    let api = Client::Lemmy(LemmyApi::with_scheme(&host, "http", http(&dir).await));

    let comment = |id: &'static str, post_id: serde_json::Value| {
        move |when: When, then: Then| {
            when.method(Method::GET)
                .path("/api/v3/comment")
                .query_param("id", id);
            then.status(200).json_body(serde_json::json!({
                "comment_view": { "comment": { "post_id": post_id } }
            }));
        }
    };
    let post = |id: &'static str, ap_id: bool| {
        let host = host.clone();
        move |when: When, then: Then| {
            when.method(Method::GET)
                .path("/api/v3/post")
                .query_param("id", id);
            let body = if ap_id {
                serde_json::json!({
                    "post_view": {
                        "counts": { "comments": 1 },
                        "post": { "ap_id": format!("http://{host}/post/{id}") },
                    },
                })
            } else {
                serde_json::json!({
                    "post_view": { "counts": { "comments": 1 }, "post": {} },
                })
            };
            then.status(200).json_body(body);
        }
    };
    let comments = |post_id: &'static str, with_ap_id: bool| {
        let host = host.clone();
        move |when: When, then: Then| {
            when.method(Method::GET)
                .path("/api/v3/comment/list")
                .query_param("post_id", post_id);
            let entry = if with_ap_id {
                serde_json::json!({ "comment": { "ap_id": format!("http://{host}/comment/77") } })
            } else {
                serde_json::json!({ "comment": {} })
            };
            then.status(200)
                .json_body(serde_json::json!({ "comments": [entry] }));
        }
    };

    // the post id arrives as a number, the context comes back complete
    server.mock_async(comment("7", serde_json::json!(5))).await;
    server.mock_async(post("5", true)).await;
    server.mock_async(comments("5", true)).await;
    let urls = api
        .fetch_context_urls("7", &format!("https://{host}/comment/7"))
        .await;
    assert_eq!(
        urls,
        vec![
            format!("http://{host}/post/5"),
            format!("http://{host}/comment/77"),
        ]
    );

    // the post id may also arrive as a string
    server
        .mock_async(comment("8", serde_json::json!("6")))
        .await;
    server.mock_async(post("6", true)).await;
    server.mock_async(comments("6", true)).await;
    let urls = api
        .fetch_context_urls("8", &format!("https://{host}/comment/8"))
        .await;
    assert_eq!(
        urls,
        vec![
            format!("http://{host}/post/6"),
            format!("http://{host}/comment/77"),
        ]
    );
}

/// A comment that names no post, and a post whose pieces cannot be read,
/// are each reported and yield no context.
#[tokio::test]
async fn a_lemmy_comment_or_post_that_cannot_be_read_yields_no_context() {
    let server = MockServer::start_async().await;
    let dir = common::tmp_dir();
    let host = host(&server);
    allow_all_robots(&server).await;
    let api = Client::Lemmy(LemmyApi::with_scheme(&host, "http", http(&dir).await));

    // the comment names no post
    server
        .mock_async(|when, then| {
            when.method(Method::GET)
                .path("/api/v3/comment")
                .query_param("id", "9");
            then.status(200)
                .json_body(serde_json::json!({ "comment_view": { "comment": {} } }));
        })
        .await;
    let urls = api
        .fetch_context_urls("9", &format!("https://{host}/comment/9"))
        .await;
    assert!(urls.is_empty(), "a comment with no post has no context");

    // the post names no ap_id, and the comment list is unreadable
    server
        .mock_async(|when, then| {
            when.method(Method::GET)
                .path("/api/v3/post")
                .query_param("id", "5");
            then.status(200).body("not json");
        })
        .await;
    server
        .mock_async(|when, then| {
            when.method(Method::GET)
                .path("/api/v3/comment/list")
                .query_param("post_id", "5");
            then.status(500);
        })
        .await;
    let urls = api
        .fetch_context_urls("5", &format!("https://{host}/post/5"))
        .await;
    assert!(urls.is_empty(), "an unreadable post has no context");
}

/// A Lemmy URL of no known type yields no context, and says so.
#[tokio::test]
async fn a_lemmy_url_of_no_known_type_yields_no_context() {
    let server = MockServer::start_async().await;
    let dir = common::tmp_dir();
    let host = host(&server);
    allow_all_robots(&server).await;
    let api = Client::Lemmy(LemmyApi::with_scheme(&host, "http", http(&dir).await));

    let urls = api
        .fetch_context_urls("5", &format!("https://{host}/weird/5"))
        .await;
    assert!(urls.is_empty());
}
