//! Lemmy, whose URLs say whether they name a community, user, post or
//! comment.

use std::sync::LazyLock;

use regex::Regex;
use serde_json::Value;
use tracing::{debug, error};

use crate::api::paging::in_pages;
use crate::http::HttpClient;
use crate::posts::Post;
use crate::translate::{anchored, first_name_in, parse_date, unusable, usable};

// the most entries this API will hand out at once
const PAGE_SIZE: i64 = 50;

static COMMUNITY_PATH: LazyLock<Regex> = LazyLock::new(|| anchored(r"^https?://[^/]+/c/"));
static USER_PATH: LazyLock<Regex> = LazyLock::new(|| anchored(r"^https?://[^/]+/u/"));

static PROFILE_PATHS: LazyLock<Vec<Regex>> =
    LazyLock::new(|| vec![anchored(r"^https?://[^/]+/(?:u|c)/(?P<name>[^/]+)")]);
static POST_PATHS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    vec![anchored(
        r"^https?://[^/]+/(?:comment|post)/(?P<name>[^/]+)",
    )]
});

/// A Lemmy post or comment, which names itself by its ActivityPub id.
pub fn parse_post(raw: &Value) -> Option<Post> {
    let ap_id = raw.get("ap_id").and_then(Value::as_str);
    let created_at = raw.get("published").and_then(parse_date);
    let (Some(ap_id), Some(created_at)) = (ap_id, created_at) else {
        unusable("lemmy", raw.get("id").unwrap_or(&Value::Null));
        return None;
    };

    // Lemmy has no visibility setting: a community is readable or it is not
    Some(Post {
        url: ap_id.to_string(),
        uri: ap_id.to_string(),
        created_at,
        is_public: true,
        reblog: None,
        in_reply_to_id: None,
        in_reply_to_account_id: None,
        reply_count: None,
        account: None,
        mentions: Vec::new(),
    })
}

/// The posts in a community listing.
fn community_entries(body: &Value) -> Option<Vec<Value>> {
    Some(
        body.get("posts")?
            .as_array()?
            .iter()
            .filter_map(|entry| entry.get("post").cloned())
            .collect(),
    )
}

/// The comments and posts in an account listing.
fn account_entries(body: &Value) -> Option<Vec<Value>> {
    let mut entries = Vec::new();
    for key in ["comments", "posts"] {
        let list = body.get(key)?.as_array()?;
        for entry in list {
            entries.push(entry.get("post")?.clone());
        }
    }
    Some(entries)
}

/// Lemmy, whose URLs say whether they name a community, user, post or
/// comment.
#[derive(Clone)]
pub struct LemmyApi {
    /// The API's base URL, scheme included: `https://host` in production.
    base: String,
    http: HttpClient,
}

impl LemmyApi {
    pub fn new(webserver: &str, http: HttpClient) -> Self {
        Self::with_scheme(webserver, "https", http)
    }

    /// A client for the API at `scheme://webserver`. Production always uses
    /// `new` (https); tests point this at a local http server.
    pub fn with_scheme(webserver: &str, scheme: &str, http: HttpClient) -> Self {
        LemmyApi {
            base: format!("{scheme}://{webserver}"),
            http,
        }
    }

    pub fn username_from(&self, profile_url: &str) -> Option<String> {
        first_name_in(&PROFILE_PATHS, profile_url)
    }

    pub fn post_id_from(&self, post_url: &str) -> Option<String> {
        first_name_in(&POST_PATHS, post_url)
    }

    pub async fn fetch_user_posts(
        &self,
        username: &str,
        profile_url: &str,
        limit: i64,
    ) -> Option<Vec<Post>> {
        if COMMUNITY_PATH.is_match(profile_url) {
            return self.fetch_community_posts(username, limit).await;
        }
        if USER_PATH.is_match(profile_url) {
            return self.fetch_account_posts(username, limit).await;
        }

        error!("Unknown Lemmy profile URL type {profile_url}");
        None
    }

    async fn fetch_community_posts(&self, username: &str, limit: i64) -> Option<Vec<Post>> {
        self.paged(
            &format!("post/list?community_name={username}"),
            community_entries,
            limit,
            &format!("community posts for community {username}"),
        )
        .await
    }

    async fn fetch_account_posts(&self, username: &str, limit: i64) -> Option<Vec<Post>> {
        // an account's page holds up to a full page of each, so a page short
        // of what was asked for has run out of both, which is what ends the
        // paging
        self.paged(
            &format!("user?username={username}"),
            account_entries,
            limit,
            &format!("user posts for user {username}"),
        )
        .await
    }

    /// Read a collection, which Lemmy numbers the pages of rather than
    /// pointing at the next one, so where we got to is a page count.
    async fn paged(
        &self,
        path: &str,
        entries: fn(&Value) -> Option<Vec<Value>>,
        limit: i64,
        describe: &str,
    ) -> Option<Vec<Post>> {
        let http = self.http.clone();
        let base = self.base.clone();
        let path = path.to_string();
        let describe = describe.to_string();
        let raw = in_pages(
            |wanted, gathered| {
                let http = http.clone();
                let base = base.clone();
                let path = path.clone();
                let describe = describe.clone();
                let page = gathered.len() as i64 / PAGE_SIZE + 1;
                async move {
                    let url = format!("{base}/api/v3/{path}&sort=New&limit={wanted}&page={page}");
                    let response = match http.get(&url, None).await {
                        Ok(response) => response,
                        Err(ex) => {
                            error!("Error getting {describe}: {ex}");
                            return None;
                        }
                    };

                    if response.status == 200 {
                        let body = match response.json() {
                            Ok(body) => body,
                            Err(ex) => {
                                error!("Error getting {describe}: {ex}");
                                return None;
                            }
                        };
                        return match entries(&body) {
                            Some(page) => Some(page),
                            None => {
                                error!("Error getting {describe}: response has no posts");
                                None
                            }
                        };
                    }

                    error!("Error getting {describe}. Status code: {}", response.status);
                    None
                }
            },
            limit,
            PAGE_SIZE,
        )
        .await;
        raw.map(|raw| usable(raw.iter().map(parse_post)))
    }

    pub async fn fetch_context_urls(&self, post_id: &str, post_url: &str) -> Vec<String> {
        if post_url.contains("/comment/") {
            return self.comment_context(post_id, post_url).await;
        }
        if post_url.contains("/post/") {
            return self.post_comments(post_id, post_url).await;
        }

        error!("unknown lemmy url type {post_url}");
        Vec::new()
    }

    /// Get the URLs of the context toots of the given toot.
    async fn comment_context(&self, comment_id: &str, post_url: &str) -> Vec<String> {
        let url = format!("{}/api/v3/comment?id={comment_id}", self.base);
        let resp = match self.http.get(&url, None).await {
            Ok(resp) => resp,
            Err(ex) => {
                error!("Error getting comment {comment_id} from {post_url}. Exception: {ex}");
                return Vec::new();
            }
        };

        if resp.status == 200 {
            let res = match resp.json() {
                Ok(res) => res,
                Err(ex) => {
                    error!("Error parsing context for comment {post_url}. Exception: {ex}");
                    return Vec::new();
                }
            };
            let post_id = res
                .get("comment_view")
                .and_then(|view| view.get("comment"))
                .and_then(|comment| comment.get("post_id"))
                .and_then(|id| id.as_i64().map(|id| id.to_string()))
                .or_else(|| {
                    res.get("comment_view")
                        .and_then(|view| view.get("comment"))
                        .and_then(|comment| comment.get("post_id"))
                        .and_then(Value::as_str)
                        .map(str::to_string)
                });
            return match post_id {
                Some(post_id) => self.post_comments(&post_id, post_url).await,
                None => {
                    error!("Error parsing context for comment {post_url}. Exception: KeyError");
                    Vec::new()
                }
            };
        }

        error!(
            "Error getting comment {comment_id} from {post_url}. Status code: {}",
            resp.status
        );
        Vec::new()
    }

    /// Get the URLs of the comments of the given post.
    async fn post_comments(&self, post_id: &str, post_url: &str) -> Vec<String> {
        let mut urls: Vec<String> = Vec::new();
        let url = format!("{}/api/v3/post?id={post_id}", self.base);
        let resp = match self.http.get(&url, None).await {
            Ok(resp) => resp,
            Err(ex) => {
                error!("Error getting post {post_id} from {post_url}. Exception: {ex}");
                return Vec::new();
            }
        };

        if resp.status == 200 {
            match resp.json() {
                Ok(res) => {
                    let comments = res
                        .get("post_view")
                        .and_then(|view| view.get("counts"))
                        .and_then(|counts| counts.get("comments"))
                        .and_then(Value::as_i64);
                    if comments == Some(0) {
                        return Vec::new();
                    }
                    match res
                        .get("post_view")
                        .and_then(|view| view.get("post"))
                        .and_then(|post| post.get("ap_id"))
                        .and_then(Value::as_str)
                    {
                        Some(ap_id) => urls.push(ap_id.to_string()),
                        None => {
                            error!(
                                "Error parsing post {post_id} from {post_url}. Exception: KeyError"
                            );
                        }
                    }
                }
                Err(ex) => {
                    error!("Error parsing post {post_id} from {post_url}. Exception: {ex}");
                }
            }
        }

        let url = format!(
            "{}/api/v3/comment/list?post_id={post_id}&sort=New&limit=50",
            self.base
        );
        let resp = match self.http.get(&url, None).await {
            Ok(resp) => resp,
            Err(ex) => {
                error!(
                    "Error getting comments for post {post_id} from {post_url}. Exception: {ex}"
                );
                return Vec::new();
            }
        };

        if resp.status == 200 {
            match resp.json() {
                Ok(res) => {
                    let parsed = res
                        .get("comments")
                        .and_then(Value::as_array)
                        .map(|comments| {
                            comments
                                .iter()
                                .map(|info| {
                                    info.get("comment")
                                        .and_then(|comment| comment.get("ap_id"))
                                        .and_then(Value::as_str)
                                        .map(str::to_string)
                                })
                                .collect::<Option<Vec<String>>>()
                        });
                    match parsed {
                        Some(Some(list_of_urls)) => {
                            debug!("Got {} comments for post {post_url}", list_of_urls.len());
                            urls.extend(list_of_urls);
                            return urls;
                        }
                        _ => {
                            error!(
                                "Error parsing comments for post {post_url}. Exception: KeyError"
                            );
                        }
                    }
                }
                Err(ex) => {
                    error!("Error parsing comments for post {post_url}. Exception: {ex}");
                }
            }
        }

        error!(
            "Error getting comments for post {post_url}. Status code: {}",
            resp.status
        );
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parse_post_reads_a_lemmy_post() {
        let raw = json!({
            "id": 42,
            "ap_id": "https://lemmy.example/post/42",
            "published": "2026-10-22T01:23:45.123Z",
        });
        let post = parse_post(&raw).unwrap();
        assert_eq!(post.url, "https://lemmy.example/post/42");
        assert_eq!(post.uri, "https://lemmy.example/post/42");
        assert!(post.is_public);
    }

    #[test]
    fn parse_post_rejects_a_post_without_ap_id_or_date() {
        assert!(parse_post(&json!({"id": 1})).is_none());
        assert!(parse_post(&json!({"ap_id": "https://lemmy.example/post/1"})).is_none());
    }

    #[tokio::test]
    async fn paths_read_names() {
        let api = LemmyApi::new(
            "lemmy.example",
            HttpClient::new(crate::config::Config::default(), crate::db::memory_pool()),
        );
        assert_eq!(
            api.username_from("https://lemmy.example/u/alice"),
            Some("alice".to_string())
        );
        assert_eq!(
            api.username_from("https://lemmy.example/c/rust"),
            Some("rust".to_string())
        );
        assert_eq!(api.username_from("https://lemmy.example/other"), None);
        assert_eq!(
            api.post_id_from("https://lemmy.example/post/123"),
            Some("123".to_string())
        );
        assert_eq!(
            api.post_id_from("https://lemmy.example/comment/456"),
            Some("456".to_string())
        );
        assert_eq!(api.post_id_from("https://lemmy.example/u/alice"), None);
    }

    #[test]
    fn entries_read_posts_out_of_listings() {
        let community = json!({"posts": [{"post": {"ap_id": "a"}}, {"post": {"ap_id": "b"}}]});
        assert_eq!(community_entries(&community).unwrap().len(), 2);
        assert!(community_entries(&json!({})).is_none());

        let account = json!({
            "comments": [{"post": {"ap_id": "c"}}],
            "posts": [{"post": {"ap_id": "p"}}, {"post": {"ap_id": "q"}}],
        });
        assert_eq!(account_entries(&account).unwrap().len(), 3);
        assert!(account_entries(&json!({"comments": []})).is_none());
    }

    /// A Lemmy client pointed at the mock server.
    fn mock_api(host: &str) -> LemmyApi {
        LemmyApi::with_scheme(
            host,
            "http",
            HttpClient::new(crate::config::Config::default(), crate::db::memory_pool()),
        )
    }

    #[tokio::test]
    async fn an_unknown_profile_url_yields_no_posts() {
        let api = mock_api("lemmy.example");
        assert!(
            api.fetch_user_posts("rust", "https://lemmy.example/other", 5)
                .await
                .is_none()
        );
    }

    #[tokio::test]
    async fn community_listing_failures_yield_no_posts() {
        // a server that is not there
        let dead = mock_api("127.0.0.1:9");
        assert!(
            dead.fetch_user_posts("rust", "https://lemmy.example/c/rust", 5)
                .await
                .is_none()
        );

        let server = httpmock::MockServer::start_async().await;
        let host = server.base_url().trim_start_matches("http://").to_string();
        let api = mock_api(&host);
        // a non-200 listing
        server
            .mock_async(|when, then| {
                when.method(httpmock::Method::GET).path("/api/v3/post/list");
                then.status(503).up_to_n_times(1);
            })
            .await;
        assert!(
            api.fetch_user_posts("rust", "https://lemmy.example/c/rust", 5)
                .await
                .is_none()
        );
        // a 200 whose body has no posts in it
        server
            .mock_async(|when, then| {
                when.method(httpmock::Method::GET).path("/api/v3/post/list");
                then.status(200)
                    .json_body(serde_json::json!({}))
                    .up_to_n_times(1);
            })
            .await;
        assert!(
            api.fetch_user_posts("rust", "https://lemmy.example/c/rust", 5)
                .await
                .is_none()
        );
        // a 200 whose body is not JSON at all
        server
            .mock_async(|when, then| {
                when.method(httpmock::Method::GET).path("/api/v3/post/list");
                then.status(200).body("<not json").up_to_n_times(1);
            })
            .await;
        assert!(
            api.fetch_user_posts("rust", "https://lemmy.example/c/rust", 5)
                .await
                .is_none()
        );
    }

    #[tokio::test]
    async fn an_unknown_context_url_type_yields_nothing() {
        let api = mock_api("lemmy.example");
        assert!(
            api.fetch_context_urls("9", "https://lemmy.example/other")
                .await
                .is_empty()
        );
    }

    #[tokio::test]
    async fn comment_context_reads_the_post_id_or_gives_up() {
        let server = httpmock::MockServer::start_async().await;
        let host = server.base_url().trim_start_matches("http://").to_string();
        let api = mock_api(&host);
        // the comment names its post as a number: the post's comments follow
        server
            .mock_async(|when, then| {
                when.method(httpmock::Method::GET).path("/api/v3/comment");
                then.status(200)
                    .json_body(serde_json::json!({
                        "comment_view": {"comment": {"post_id": 42}}
                    }))
                    .up_to_n_times(1);
            })
            .await;
        server
            .mock_async(|when, then| {
                when.method(httpmock::Method::GET).path("/api/v3/post");
                then.status(200)
                    .json_body(serde_json::json!({
                        "post_view": {"counts": {"comments": 0}}
                    }))
                    .up_to_n_times(1);
            })
            .await;
        server
            .mock_async(|when, then| {
                when.method(httpmock::Method::GET)
                    .path("/api/v3/comment/list");
                then.status(200)
                    .json_body(serde_json::json!({
                        "comments": [{"comment": {"ap_id": "https://lemmy.example/post/42"}}]
                    }))
                    .up_to_n_times(1);
            })
            .await;
        let urls = api
            .fetch_context_urls("9", "https://lemmy.example/comment/9")
            .await;
        assert_eq!(
            urls,
            Vec::<String>::new(),
            "a post with no comments has no context"
        );

        // a comment answer without a post_id: the context ends there
        server
            .mock_async(|when, then| {
                when.method(httpmock::Method::GET).path("/api/v3/comment");
                then.status(200)
                    .json_body(serde_json::json!({"comment_view": {}}));
            })
            .await;
        assert!(
            api.fetch_context_urls("9", "https://lemmy.example/comment/9")
                .await
                .is_empty()
        );

        // a non-200 comment answer
        server
            .mock_async(|when, then| {
                when.method(httpmock::Method::GET).path("/api/v3/comment");
                then.status(500);
            })
            .await;
        assert!(
            api.fetch_context_urls("9", "https://lemmy.example/comment/9")
                .await
                .is_empty()
        );
    }

    #[tokio::test]
    async fn post_context_handles_every_answer_shape() {
        let server = httpmock::MockServer::start_async().await;
        let host = server.base_url().trim_start_matches("http://").to_string();
        let api = mock_api(&host);
        // the post's own ap_id, plus its comments' ap_ids
        server
            .mock_async(|when, then| {
                when.method(httpmock::Method::GET).path("/api/v3/post");
                then.status(200).json_body(serde_json::json!({
                    "post_view": {"counts": {"comments": 2}, "post": {"ap_id": "https://lemmy.example/post/42"}}
                })).up_to_n_times(1);
            })
            .await;
        server
            .mock_async(|when, then| {
                when.method(httpmock::Method::GET)
                    .path("/api/v3/comment/list");
                then.status(200)
                    .json_body(serde_json::json!({
                        "comments": [{"comment": {"ap_id": "https://lemmy.example/comment/1"}}]
                    }))
                    .up_to_n_times(1);
            })
            .await;
        let urls = api
            .fetch_context_urls("42", "https://lemmy.example/post/42")
            .await;
        assert_eq!(
            urls,
            vec![
                "https://lemmy.example/post/42".to_string(),
                "https://lemmy.example/comment/1".to_string()
            ]
        );

        // a post with no comments at all: just the ap_id, no comment fetch
        server
            .mock_async(|when, then| {
                when.method(httpmock::Method::GET).path("/api/v3/post");
                then.status(200).json_body(serde_json::json!({
                    "post_view": {"counts": {"comments": 0}, "post": {"ap_id": "https://lemmy.example/post/43"}}
                })).up_to_n_times(1);
            })
            .await;
        let urls = api
            .fetch_context_urls("43", "https://lemmy.example/post/43")
            .await;
        assert_eq!(urls, Vec::<String>::new(), "no comments, no context");

        // a post answer without an ap_id: noted, no urls from it
        server
            .mock_async(|when, then| {
                when.method(httpmock::Method::GET).path("/api/v3/post");
                then.status(200)
                    .json_body(serde_json::json!({
                        "post_view": {"counts": {"comments": 1}}
                    }))
                    .up_to_n_times(1);
            })
            .await;
        server
            .mock_async(|when, then| {
                when.method(httpmock::Method::GET)
                    .path("/api/v3/comment/list");
                then.status(200)
                    .json_body(serde_json::json!({"comments": []}));
            })
            .await;
        assert!(
            api.fetch_context_urls("44", "https://lemmy.example/post/44")
                .await
                .is_empty()
        );

        // a post answer that is not JSON
        server
            .mock_async(|when, then| {
                when.method(httpmock::Method::GET).path("/api/v3/post");
                then.status(200).body("<not json").up_to_n_times(1);
            })
            .await;
        assert!(
            api.fetch_context_urls("45", "https://lemmy.example/post/45")
                .await
                .is_empty()
        );

        // a post that is not there
        server
            .mock_async(|when, then| {
                when.method(httpmock::Method::GET).path("/api/v3/post");
                then.status(500);
            })
            .await;
        assert!(
            api.fetch_context_urls("46", "https://lemmy.example/post/46")
                .await
                .is_empty()
        );

        // a server that is not there at all
        let dead = mock_api("127.0.0.1:9");
        assert!(
            dead.fetch_context_urls("47", "https://lemmy.example/post/47")
                .await
                .is_empty()
        );
    }

    #[tokio::test]
    async fn a_comment_listing_without_ap_ids_or_that_fails_is_empty() {
        let server = httpmock::MockServer::start_async().await;
        let host = server.base_url().trim_start_matches("http://").to_string();
        let api = mock_api(&host);
        // comments without ap_ids: the parse fails, the fetch is empty
        server
            .mock_async(|when, then| {
                when.method(httpmock::Method::GET).path("/api/v3/post");
                then.status(200).json_body(serde_json::json!({
                    "post_view": {"counts": {"comments": 1}, "post": {"ap_id": "https://lemmy.example/post/50"}}
                })).up_to_n_times(1);
            })
            .await;
        server
            .mock_async(|when, then| {
                when.method(httpmock::Method::GET)
                    .path("/api/v3/comment/list");
                then.status(200).json_body(serde_json::json!({
                    "comments": [{"comment": {}}]
                }));
            })
            .await;
        assert!(
            api.fetch_context_urls("50", "https://lemmy.example/post/50")
                .await
                .is_empty()
        );

        // a comment listing that is not JSON
        server
            .mock_async(|when, then| {
                when.method(httpmock::Method::GET).path("/api/v3/post");
                then.status(200).json_body(serde_json::json!({
                    "post_view": {"counts": {"comments": 1}, "post": {"ap_id": "https://lemmy.example/post/51"}}
                })).up_to_n_times(1);
            })
            .await;
        server
            .mock_async(|when, then| {
                when.method(httpmock::Method::GET)
                    .path("/api/v3/comment/list");
                then.status(200).body("<not json");
            })
            .await;
        assert!(
            api.fetch_context_urls("51", "https://lemmy.example/post/51")
                .await
                .is_empty()
        );

        // a comment listing that is not there
        server
            .mock_async(|when, then| {
                when.method(httpmock::Method::GET).path("/api/v3/post");
                then.status(200).json_body(serde_json::json!({
                    "post_view": {"counts": {"comments": 1}, "post": {"ap_id": "https://lemmy.example/post/52"}}
                })).up_to_n_times(1);
            })
            .await;
        server
            .mock_async(|when, then| {
                when.method(httpmock::Method::GET)
                    .path("/api/v3/comment/list");
                then.status(500);
            })
            .await;
        assert!(
            api.fetch_context_urls("52", "https://lemmy.example/post/52")
                .await
                .is_empty()
        );
    }
}
