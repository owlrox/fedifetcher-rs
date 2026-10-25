//! PeerTube, where posts are videos and replies are comment threads.

use std::sync::LazyLock;

use regex::Regex;
use serde_json::Value;
use tracing::error;

use crate::api::paging::in_pages;
use crate::http::HttpClient;
use crate::posts::Post;
use crate::translate::{anchored, first_name_in, non_empty_str, parse_date, unusable, usable};

// PeerTube says who may watch with a number: the rest of its VideoPrivacy
// enum is 3 private, 4 internal and 5 password protected
const PUBLIC: &[i64] = &[1, 2]; // public, unlisted

// the most videos this API will hand out at once
const PAGE_SIZE: i64 = 100;

static CHANNEL_PATH: LazyLock<Regex> =
    LazyLock::new(|| anchored(r"^https?://[^/]+/(?:video-channels|c)/"));

static PROFILE_PATHS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    vec![anchored(
        r"^https?://[^/]+/(?:accounts|a|video-channels|c)/(?P<name>[^/]+)",
    )]
});

static POST_PATHS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    vec![
        anchored(r"^https?://[^/]+/videos/watch/(?P<name>[^/]+)"),
        // the Python pattern refuses /w/p/... with a (?!p/) lookahead, which
        // this regex engine does not have, so the refusal is checked in
        // post_id_from instead
        anchored(r"^https?://[^/]+/w/(?P<name>[^/]+)"),
    ]
});

/// A PeerTube video, whose comments are the replies to it.
pub fn parse_post(raw: &Value) -> Option<Post> {
    let url = raw.get("url").and_then(Value::as_str);
    let created_at = raw
        .get("publishedAt")
        .filter(|value| !value.is_null())
        .and_then(parse_date)
        .or_else(|| {
            raw.get("createdAt")
                .filter(|value| !value.is_null())
                .and_then(parse_date)
        });
    let (Some(url), Some(created_at)) = (url, created_at) else {
        unusable("peertube", raw.get("uuid").unwrap_or(&Value::Null));
        return None;
    };

    let privacy_id = raw
        .get("privacy")
        .and_then(|privacy| privacy.get("id"))
        .and_then(Value::as_i64)
        .unwrap_or(1);
    Some(Post {
        url: url.to_string(),
        uri: non_empty_str(raw.get("uri")).unwrap_or_else(|| url.to_string()),
        created_at,
        is_public: PUBLIC.contains(&privacy_id),
        reblog: None,
        in_reply_to_id: None,
        in_reply_to_account_id: None,
        reply_count: None,
        account: None,
        mentions: Vec::new(),
    })
}

/// PeerTube, where posts are videos and replies are comment threads.
#[derive(Clone)]
pub struct PeerTubeApi {
    webserver: String,
    /// The API's base URL, scheme included: `https://host` in production.
    base: String,
    http: HttpClient,
}

impl PeerTubeApi {
    pub fn new(webserver: &str, http: HttpClient) -> Self {
        Self::with_scheme(webserver, "https", http)
    }

    /// A client for the API at `scheme://webserver`. Production always uses
    /// `new` (https); tests point this at a local http server.
    pub fn with_scheme(webserver: &str, scheme: &str, http: HttpClient) -> Self {
        PeerTubeApi {
            webserver: webserver.to_string(),
            base: format!("{scheme}://{webserver}"),
            http,
        }
    }

    pub fn username_from(&self, profile_url: &str) -> Option<String> {
        first_name_in(&PROFILE_PATHS, profile_url)
    }

    pub fn post_id_from(&self, post_url: &str) -> Option<String> {
        for (index, pattern) in POST_PATHS.iter().enumerate() {
            if let Some(captures) = pattern.captures(post_url) {
                let name = captures.name("name")?.as_str();
                if index == 1 {
                    // the Python pattern's (?!p/) lookahead: /w/p/... is a
                    // playlist, not a video
                    let after = &post_url[captures.get(0).unwrap().end()..];
                    if name == "p" && after.starts_with('/') {
                        return None;
                    }
                }
                return Some(name.to_string());
            }
        }
        None
    }

    pub async fn fetch_user_posts(
        &self,
        username: &str,
        profile_url: &str,
        limit: i64,
    ) -> Option<Vec<Post>> {
        // a channel and the account that owns it are different things here,
        // and they are asked for separately. Anything we do not recognize is
        // tried as an account, which is what every URL used to be treated as.
        let collection = if CHANNEL_PATH.is_match(profile_url) {
            "video-channels"
        } else {
            "accounts"
        };

        let http = self.http.clone();
        let base = self.base.clone();
        let webserver = self.webserver.clone();
        let username = username.to_string();
        let collection = collection.to_string();
        let raw = in_pages(
            |wanted, gathered| {
                let http = http.clone();
                let base = base.clone();
                let webserver = webserver.clone();
                let username = username.clone();
                let collection = collection.clone();
                let start = gathered.len() as i64;
                async move {
                    let url = format!(
                        "{base}/api/v1/{collection}/{username}/videos?start={start}&count={wanted}"
                    );
                    let response = match http.get(&url, None).await {
                        Ok(response) => response,
                        Err(ex) => {
                            error!(
                                "Error getting posts by user {username} from {webserver}. Exception: {ex}"
                            );
                            return None;
                        }
                    };
                    if response.status == 200 {
                        return match response.json() {
                            Ok(body) => body.get("data").and_then(Value::as_array).cloned(),
                            _ => None,
                        };
                    }
                    error!(
                        "Error getting posts by user {username} from {webserver}. Status Code: {}",
                        response.status
                    );
                    None
                }
            },
            limit,
            PAGE_SIZE,
        )
        .await;
        raw.map(|raw| usable(raw.iter().map(parse_post)))
    }

    /// Get the URLs of the comments of a given peertube video.
    pub async fn fetch_context_urls(&self, post_id: &str, post_url: &str) -> Vec<String> {
        let url = format!("{}/api/v1/videos/{post_id}/comment-threads", self.base);
        let resp = match self.http.get(&url, None).await {
            Ok(resp) => resp,
            Err(ex) => {
                error!(
                    "Error getting comments on video {post_id} from {post_url}. Exception: {ex}"
                );
                return Vec::new();
            }
        };

        if resp.status == 200 {
            return match resp.json() {
                Ok(body) => match body.get("data").and_then(Value::as_array) {
                    Some(comments) => usable(comments.iter().map(|comment| {
                        comment
                            .get("url")
                            .and_then(Value::as_str)
                            .map(str::to_string)
                    })),
                    None => {
                        error!(
                            "Error parsing comments on video {post_id} from {post_url}. Exception: KeyError"
                        );
                        Vec::new()
                    }
                },
                Err(ex) => {
                    error!(
                        "Error parsing comments on video {post_id} from {post_url}. Exception: {ex}"
                    );
                    Vec::new()
                }
            };
        }

        error!(
            "Error getting comments on video {post_id} from {post_url}. Status code: {}",
            resp.status
        );
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn api() -> PeerTubeApi {
        PeerTubeApi::new(
            "peertube.example",
            HttpClient::new(crate::config::Config::default(), crate::db::memory_pool()),
        )
    }

    #[test]
    fn parse_post_reads_a_video() {
        let raw = json!({
            "uuid": "abc",
            "url": "https://peertube.example/w/abc",
            "publishedAt": "2026-10-22T01:23:45.123Z",
            "privacy": {"id": 2},
        });
        let post = parse_post(&raw).unwrap();
        assert_eq!(post.url, "https://peertube.example/w/abc");
        assert!(post.is_public);
    }

    #[test]
    fn parse_post_falls_back_to_created_at_and_hides_private_videos() {
        let raw = json!({
            "url": "https://peertube.example/w/abc",
            "createdAt": "2026-10-22T01:23:45Z",
            "privacy": {"id": 3},
        });
        let post = parse_post(&raw).unwrap();
        assert!(!post.is_public);

        // privacy is optional, and public is the default
        let raw = json!({
            "url": "https://peertube.example/w/abc",
            "createdAt": "2026-10-22T01:23:45Z",
        });
        assert!(parse_post(&raw).unwrap().is_public);
    }

    #[test]
    fn parse_post_rejects_a_video_without_url_or_date() {
        assert!(parse_post(&json!({"uuid": "abc"})).is_none());
        assert!(parse_post(&json!({"url": "https://peertube.example/w/abc"})).is_none());
    }

    #[tokio::test]
    async fn paths_read_names() {
        let api = api();
        assert_eq!(
            api.username_from("https://peertube.example/accounts/alice"),
            Some("alice".to_string())
        );
        assert_eq!(
            api.username_from("https://peertube.example/a/bob"),
            Some("bob".to_string())
        );
        assert_eq!(
            api.username_from("https://peertube.example/video-channels/chan"),
            Some("chan".to_string())
        );
        assert_eq!(
            api.username_from("https://peertube.example/c/dan"),
            Some("dan".to_string())
        );
        assert_eq!(api.username_from("https://peertube.example/other"), None);
    }

    #[tokio::test]
    async fn post_id_reads_watch_and_w_urls() {
        let api = api();
        assert_eq!(
            api.post_id_from("https://peertube.example/videos/watch/abc"),
            Some("abc".to_string())
        );
        assert_eq!(
            api.post_id_from("https://peertube.example/w/def"),
            Some("def".to_string())
        );
        // a bare /w/p is a video called p; /w/p/... is a playlist
        assert_eq!(
            api.post_id_from("https://peertube.example/w/p"),
            Some("p".to_string())
        );
        assert_eq!(api.post_id_from("https://peertube.example/w/p/xyz"), None);
        assert_eq!(
            api.post_id_from("https://peertube.example/w/playlist"),
            Some("playlist".to_string())
        );
        assert_eq!(
            api.post_id_from("https://peertube.example/accounts/alice"),
            None
        );
    }

    #[test]
    fn channel_paths_pick_the_collection() {
        assert!(CHANNEL_PATH.is_match("https://peertube.example/video-channels/chan"));
        assert!(CHANNEL_PATH.is_match("https://peertube.example/c/dan"));
        assert!(!CHANNEL_PATH.is_match("https://peertube.example/accounts/alice"));
    }

    /// A PeerTube client pointed at the mock server.
    fn mock_api(host: &str) -> PeerTubeApi {
        PeerTubeApi::with_scheme(
            host,
            "http",
            HttpClient::new(crate::config::Config::default(), crate::db::memory_pool()),
        )
    }

    #[tokio::test]
    async fn post_id_from_refuses_a_playlist_where_a_video_was_expected() {
        let api = mock_api("peertube.example");
        // /w/p/... is a playlist, not a video
        assert_eq!(api.post_id_from("https://peertube.example/w/p/abc"), None);
        assert_eq!(
            api.post_id_from("https://peertube.example/w/abc"),
            Some("abc".to_string())
        );
    }

    #[tokio::test]
    async fn video_listing_failures_yield_no_posts() {
        // a server that is not there
        let dead = mock_api("127.0.0.1:9");
        assert!(
            dead.fetch_user_posts("alice", "https://peertube.example/a/alice", 5)
                .await
                .is_none()
        );

        let server = httpmock::MockServer::start_async().await;
        let host = server.base_url().trim_start_matches("http://").to_string();
        let api = mock_api(&host);
        // a non-200 listing
        server
            .mock_async(|when, then| {
                when.method(httpmock::Method::GET)
                    .path("/api/v1/accounts/alice/videos");
                then.status(503).up_to_n_times(1);
            })
            .await;
        assert!(
            api.fetch_user_posts("alice", "https://peertube.example/a/alice", 5)
                .await
                .is_none()
        );
        // a 200 whose body has no data in it
        server
            .mock_async(|when, then| {
                when.method(httpmock::Method::GET)
                    .path("/api/v1/accounts/alice/videos");
                then.status(200)
                    .json_body(serde_json::json!({}))
                    .up_to_n_times(1);
            })
            .await;
        assert!(
            api.fetch_user_posts("alice", "https://peertube.example/a/alice", 5)
                .await
                .is_none()
        );
    }

    #[tokio::test]
    async fn comment_threads_handle_every_answer_shape() {
        let server = httpmock::MockServer::start_async().await;
        let host = server.base_url().trim_start_matches("http://").to_string();
        let api = mock_api(&host);
        // comments with urls, and without
        server
            .mock_async(|when, then| {
                when.method(httpmock::Method::GET)
                    .path("/api/v1/videos/9/comment-threads");
                then.status(200)
                    .json_body(serde_json::json!({
                        "data": [{"url": "https://peertube.example/w/abc"}, {}]
                    }))
                    .up_to_n_times(1);
            })
            .await;
        let urls = api
            .fetch_context_urls("9", "https://peertube.example/w/abc")
            .await;
        assert_eq!(urls, vec!["https://peertube.example/w/abc".to_string()]);

        // a 200 whose body has no data
        server
            .mock_async(|when, then| {
                when.method(httpmock::Method::GET)
                    .path("/api/v1/videos/9/comment-threads");
                then.status(200)
                    .json_body(serde_json::json!({}))
                    .up_to_n_times(1);
            })
            .await;
        assert!(
            api.fetch_context_urls("9", "https://peertube.example/w/abc")
                .await
                .is_empty()
        );

        // a 200 that is not JSON
        server
            .mock_async(|when, then| {
                when.method(httpmock::Method::GET)
                    .path("/api/v1/videos/9/comment-threads");
                then.status(200).body("<not json").up_to_n_times(1);
            })
            .await;
        assert!(
            api.fetch_context_urls("9", "https://peertube.example/w/abc")
                .await
                .is_empty()
        );

        // a non-200
        server
            .mock_async(|when, then| {
                when.method(httpmock::Method::GET)
                    .path("/api/v1/videos/9/comment-threads");
                then.status(500);
            })
            .await;
        assert!(
            api.fetch_context_urls("9", "https://peertube.example/w/abc")
                .await
                .is_empty()
        );

        // a server that is not there
        let dead = mock_api("127.0.0.1:9");
        assert!(
            dead.fetch_context_urls("9", "https://peertube.example/w/abc")
                .await
                .is_empty()
        );
    }
}
