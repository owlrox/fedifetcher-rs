//! Misskey and its forks: Calckey, Firefish, Foundkey, Sharkey.

use std::sync::LazyLock;

use regex::Regex;
use serde_json::Value;
use tracing::{debug, error};

use crate::api::paging::in_pages;
use crate::http::HttpClient;
use crate::posts::Post;
use crate::translate::{anchored, first_name_in, non_empty_str, parse_date, unusable, usable};

// "home" is Misskey's unlisted: public, but kept off the public timelines
const PUBLIC: &[&str] = &["public", "home"];

// the most notes this API will hand out at once
const PAGE_SIZE: i64 = 100;

static PROFILE_PATHS: LazyLock<Vec<Regex>> =
    LazyLock::new(|| vec![anchored(r"^https?://[^/]+/@(?P<name>[^/]+)")]);
static POST_PATHS: LazyLock<Vec<Regex>> =
    LazyLock::new(|| vec![anchored(r"^https?://[^/]+/notes/(?P<name>[^/]+)")]);

/// A Misskey note, which only has a URL of its own when it is a copy.
pub fn parse_post(raw: &Value, base: &str) -> Option<Post> {
    let note_id = raw.get("id").and_then(Value::as_str);
    let created_at = raw.get("createdAt").and_then(parse_date);
    let (Some(note_id), Some(created_at)) = (note_id, created_at) else {
        unusable("misskey", raw.get("id").unwrap_or(&Value::Null));
        return None;
    };

    let url = non_empty_str(raw.get("url")).unwrap_or_else(|| format!("{base}/notes/{note_id}"));
    let renote = raw
        .get("renote")
        .filter(|renote| !renote.is_null())
        .and_then(|renote| parse_post(renote, base));

    Some(Post {
        url: url.clone(),
        uri: non_empty_str(raw.get("uri")).unwrap_or(url),
        created_at,
        is_public: raw
            .get("visibility")
            .and_then(Value::as_str)
            .map(|visibility| PUBLIC.contains(&visibility))
            .unwrap_or(false),
        reblog: renote.map(Box::new),
        in_reply_to_id: raw
            .get("replyId")
            .and_then(Value::as_str)
            .map(str::to_string),
        in_reply_to_account_id: None,
        reply_count: raw.get("repliesCount").and_then(Value::as_i64),
        account: None,
        mentions: Vec::new(),
    })
}

/// Misskey and its forks: Calckey, Firefish, Foundkey, Sharkey.
#[derive(Clone)]
pub struct MisskeyApi {
    webserver: String,
    /// The API's base URL, scheme included: `https://host` in production.
    base: String,
    http: HttpClient,
}

impl MisskeyApi {
    pub fn new(webserver: &str, http: HttpClient) -> Self {
        Self::with_scheme(webserver, "https", http)
    }

    /// A client for the API at `scheme://webserver`. Production always uses
    /// `new` (https); tests point this at a local http server.
    pub fn with_scheme(webserver: &str, scheme: &str, http: HttpClient) -> Self {
        MisskeyApi {
            webserver: webserver.to_string(),
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
        _profile_url: &str,
        limit: i64,
    ) -> Option<Vec<Post>> {
        let user_id = self.find_user_id(username).await?;

        let http = self.http.clone();
        let base = self.base.clone();
        let webserver = self.webserver.clone();
        let username = username.to_string();
        let raw = in_pages(
            |wanted, gathered| {
                let http = http.clone();
                let base = base.clone();
                let webserver = webserver.clone();
                let username = username.clone();
                let user_id = user_id.clone();
                // everything older than the oldest note we already have
                let until_id = gathered
                    .last()
                    .map(|last| last.get("id").and_then(Value::as_str).map(str::to_string));
                async move {
                    match until_id {
                        Some(Some(id)) => {
                            notes(
                                &http,
                                &base,
                                &webserver,
                                &user_id,
                                &username,
                                wanted,
                                Some(&id),
                            )
                            .await
                        }
                        Some(None) => {
                            error!("Error getting posts for user {username}: note has no id");
                            None
                        }
                        None => {
                            notes(&http, &base, &webserver, &user_id, &username, wanted, None).await
                        }
                    }
                }
            },
            limit,
            PAGE_SIZE,
        )
        .await;
        let base = self.base.clone();
        raw.map(|raw| usable(raw.iter().map(|note| parse_post(note, &base))))
    }

    async fn find_user_id(&self, username: &str) -> Option<String> {
        // query user info via search api
        // we could filter by host but there's no way to limit that to just the
        // main host on firefish currently; on misskey it works if you supply
        // '.' as the host but firefish does not
        let url = format!("{}/api/users/search-by-username-and-host", self.base);
        let resp = match self
            .http
            .post(&url, &serde_json::json!({ "username": username }), None)
            .await
        {
            Ok(resp) => resp,
            Err(ex) => {
                error!(
                    "Error finding user {username} from {}. Exception: {ex}",
                    self.webserver
                );
                return None;
            }
        };

        if resp.status != 200 {
            error!(
                "Error finding user {username} from {}. Status Code: {}",
                self.webserver, resp.status
            );
            return None;
        }

        let body = match resp.json() {
            Ok(body) => body,
            Err(ex) => {
                error!(
                    "Error finding user {username} from {}. Exception: {ex}",
                    self.webserver
                );
                return None;
            }
        };
        let Some(users) = body.as_array() else {
            error!(
                "Error finding user {username} from {}. Exception: response is not a list",
                self.webserver
            );
            return None;
        };

        for user in users {
            match user.get("host") {
                Some(Value::Null) => {
                    return user.get("id").and_then(Value::as_str).map(str::to_string);
                }
                Some(_) => continue,
                // Python reads user['host'] directly, so a user without one
                // fails the whole search
                None => {
                    error!(
                        "Error finding user {username} from {}. Exception: KeyError: 'host'",
                        self.webserver
                    );
                    return None;
                }
            }
        }

        error!(
            "Error finding user {username} from {}: user not found on server in search",
            self.webserver
        );
        None
    }

    /// Get the URLs of the comments of a given misskey post.
    pub async fn fetch_context_urls(&self, post_id: &str, post_url: &str) -> Vec<String> {
        let mut urls: Vec<String> = Vec::new();
        for (endpoint, params) in [
            (
                "children",
                serde_json::json!({ "noteId": post_id, "limit": 100, "depth": 12 }),
            ),
            (
                "conversation",
                serde_json::json!({ "noteId": post_id, "limit": 100 }),
            ),
        ] {
            let url = format!("{}/api/notes/{endpoint}", self.base);
            let resp = match self.http.post(&url, &params, None).await {
                Ok(resp) => resp,
                Err(ex) => {
                    error!("Error getting post {post_id} from {post_url}. Exception: {ex}");
                    return Vec::new();
                }
            };

            if resp.status != 200 {
                error!(
                    "Error getting post {post_id} from {post_url}. Status Code: {}",
                    resp.status
                );
                continue;
            }

            match resp.json() {
                Ok(res) => {
                    debug!("Got {endpoint} for misskey post {post_url}");
                    if let Some(notes) = res.as_array() {
                        for note in notes {
                            if let Some(id) = note.get("id").and_then(Value::as_str) {
                                urls.push(format!("{}/notes/{id}", self.base));
                            }
                        }
                    }
                }
                Err(ex) => {
                    error!("Error parsing post {post_id} from {post_url}. Exception: {ex}");
                }
            }
        }

        urls
    }
}

async fn notes(
    http: &HttpClient,
    base: &str,
    webserver: &str,
    user_id: &str,
    username: &str,
    wanted: i64,
    until_id: Option<&str>,
) -> Option<Vec<Value>> {
    let mut params = serde_json::json!({ "userId": user_id, "limit": wanted });
    if let Some(until_id) = until_id {
        params["untilId"] = Value::String(until_id.to_string());
    }

    let url = format!("{base}/api/users/notes");
    let resp = match http.post(&url, &params, None).await {
        Ok(resp) => resp,
        Err(ex) => {
            error!("Error getting posts by user {username} from {webserver}. Exception: {ex}");
            return None;
        }
    };

    if resp.status == 200 {
        return match resp.json() {
            Ok(Value::Array(notes)) => Some(notes),
            _ => None,
        };
    }

    error!(
        "Error getting posts by user {username} from {webserver}. Status Code: {}",
        resp.status
    );
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parse_post_reads_a_note() {
        let raw = json!({
            "id": "9abc",
            "createdAt": "2026-10-22T01:23:45.123Z",
            "visibility": "home",
            "replyId": "8def",
            "repliesCount": 2,
        });
        let post = parse_post(&raw, "https://misskey.example").unwrap();
        assert_eq!(post.url, "https://misskey.example/notes/9abc");
        assert_eq!(post.uri, "https://misskey.example/notes/9abc");
        assert!(post.is_public);
        assert_eq!(post.in_reply_to_id.as_deref(), Some("8def"));
        assert_eq!(post.reply_count, Some(2));
    }

    #[test]
    fn parse_post_prefers_the_notes_own_url_and_reads_renotes() {
        let raw = json!({
            "id": "1",
            "createdAt": "2026-10-22T01:23:45Z",
            "url": "https://elsewhere.example/notes/1",
            "uri": "https://misskey.example/notes/1",
            "visibility": "followers",
            "renote": {
                "id": "2",
                "createdAt": "2026-10-21T01:23:45Z",
                "visibility": "public",
            },
        });
        let post = parse_post(&raw, "https://misskey.example").unwrap();
        assert_eq!(post.url, "https://elsewhere.example/notes/1");
        assert!(!post.is_public);
        assert!(post.is_boost());
        assert_eq!(post.original().url, "https://misskey.example/notes/2");
        assert!(post.original().is_public);
    }

    #[test]
    fn parse_post_rejects_a_note_without_id_or_date() {
        assert!(
            parse_post(
                &json!({"createdAt": "2026-10-22T01:23:45Z"}),
                "https://misskey.example"
            )
            .is_none()
        );
        assert!(parse_post(&json!({"id": "1"}), "https://misskey.example").is_none());
    }

    #[tokio::test]
    async fn paths_read_names() {
        let api = MisskeyApi::new(
            "misskey.example",
            HttpClient::new(crate::config::Config::default(), crate::db::memory_pool()),
        );
        assert_eq!(
            api.username_from("https://misskey.example/@alice"),
            Some("alice".to_string())
        );
        assert_eq!(api.username_from("https://misskey.example/users/bob"), None);
        assert_eq!(
            api.post_id_from("https://misskey.example/notes/9abc"),
            Some("9abc".to_string())
        );
        assert_eq!(api.post_id_from("https://misskey.example/@alice"), None);
    }

    #[tokio::test]
    async fn a_user_search_failure_is_reported_and_yields_no_posts() {
        let server = httpmock::MockServer::start_async().await;
        let host = server.base_url().trim_start_matches("http://").to_string();
        let api = MisskeyApi::with_scheme(
            &host,
            "http",
            HttpClient::new(crate::config::Config::default(), crate::db::memory_pool()),
        );
        server
            .mock_async(|when, then| {
                when.method(httpmock::Method::POST)
                    .path("/api/users/search-by-username-and-host");
                then.status(500);
            })
            .await;
        assert!(api.fetch_user_posts("alice", "", 5).await.is_none());
    }

    #[tokio::test]
    async fn a_user_search_that_is_not_a_list_or_lacks_a_host_yields_no_posts() {
        let server = httpmock::MockServer::start_async().await;
        let host = server.base_url().trim_start_matches("http://").to_string();
        let api = MisskeyApi::with_scheme(
            &host,
            "http",
            HttpClient::new(crate::config::Config::default(), crate::db::memory_pool()),
        );
        // a JSON object instead of the list of users
        server
            .mock_async(|when, then| {
                when.method(httpmock::Method::POST)
                    .path("/api/users/search-by-username-and-host");
                then.status(200).json_body(serde_json::json!({}));
            })
            .await;
        assert!(api.fetch_user_posts("alice", "", 5).await.is_none());

        // a user entry without a host key fails the whole search
        server
            .mock_async(|when, then| {
                when.method(httpmock::Method::POST)
                    .path("/api/users/search-by-username-and-host");
                then.status(200).json_body(serde_json::json!([{"id": "7"}]));
            })
            .await;
        assert!(api.fetch_user_posts("alice", "", 5).await.is_none());

        // a user on another host is skipped, and the search ends empty
        server
            .mock_async(|when, then| {
                when.method(httpmock::Method::POST)
                    .path("/api/users/search-by-username-and-host");
                then.status(200)
                    .json_body(serde_json::json!([{"id": "7", "host": "other.example"}]));
            })
            .await;
        assert!(api.fetch_user_posts("alice", "", 5).await.is_none());
    }

    #[tokio::test]
    async fn notes_that_fill_the_page_are_paged_with_untilid() {
        let server = httpmock::MockServer::start_async().await;
        let host = server.base_url().trim_start_matches("http://").to_string();
        let api = MisskeyApi::with_scheme(
            &host,
            "http",
            HttpClient::new(crate::config::Config::default(), crate::db::memory_pool()),
        );
        server
            .mock_async(|when, then| {
                when.method(httpmock::Method::POST)
                    .path("/api/users/search-by-username-and-host");
                then.status(200)
                    .json_body(serde_json::json!([{"id": "7", "host": null}]));
            })
            .await;
        // a full first page (the pager only asks again when the page was
        // filled) and a limit beyond it
        let notes = |ids: Vec<String>| {
            serde_json::Value::Array(
                ids.iter()
                    .map(|id| {
                        serde_json::json!({"id": id, "createdAt": "2026-10-01T00:00:00Z", "visibility": "public"})
                    })
                    .collect(),
            )
        };
        let first_page: Vec<String> = (1..=100).map(|n| format!("n{n:03}")).collect();
        let last_of_first = first_page.last().unwrap().clone();
        let first = server
            .mock_async(|when, then| {
                when.method(httpmock::Method::POST).path("/api/users/notes");
                then.status(200)
                    .json_body(notes(first_page))
                    .up_to_n_times(1);
            })
            .await;
        // second page: fewer than wanted, so the pager stops. The first
        // mock retires after its one use, so only the resume request —
        // carrying untilId from the first page's last note — lands here.
        let second = server
            .mock_async(move |when, then| {
                when.method(httpmock::Method::POST)
                    .path("/api/users/notes")
                    .body_includes(format!("\"untilId\":\"{last_of_first}\""));
                then.status(200)
                    .json_body(notes((101..=150).map(|n| format!("n{n:03}")).collect()));
            })
            .await;
        let posts = api.fetch_user_posts("alice", "", 150).await.unwrap();
        assert_eq!(posts.len(), 150, "two pages gathered");
        assert_eq!(first.calls(), 1, "the first page without untilId");
        assert_eq!(
            second.calls(),
            1,
            "the second page resumes from the first page's last note"
        );
    }

    #[tokio::test]
    async fn a_page_whose_last_note_has_no_id_stops_the_paging() {
        let server = httpmock::MockServer::start_async().await;
        let host = server.base_url().trim_start_matches("http://").to_string();
        let api = MisskeyApi::with_scheme(
            &host,
            "http",
            HttpClient::new(crate::config::Config::default(), crate::db::memory_pool()),
        );
        server
            .mock_async(|when, then| {
                when.method(httpmock::Method::POST)
                    .path("/api/users/search-by-username-and-host");
                then.status(200)
                    .json_body(serde_json::json!([{"id": "7", "host": null}]));
            })
            .await;
        // a full page whose last note carries no id: the pager cannot know
        // where to resume, so it stops with what it has
        let page: Vec<Value> = (1..=100)
            .map(|n| {
                if n == 100 {
                    serde_json::json!({"createdAt": "2026-10-02T00:00:00Z", "visibility": "public"})
                } else {
                    serde_json::json!({"id": format!("n{n:03}"), "createdAt": "2026-10-01T00:00:00Z", "visibility": "public"})
                }
            })
            .collect();
        server
            .mock_async(|when, then| {
                when.method(httpmock::Method::POST).path("/api/users/notes");
                then.status(200).json_body(page);
            })
            .await;
        let posts = api.fetch_user_posts("alice", "", 104).await.unwrap();
        assert_eq!(
            posts.len(),
            99,
            "the idless note is unusable: {}",
            posts.len()
        );
    }

    #[tokio::test]
    async fn a_notes_request_that_fails_or_is_not_a_list_yields_none() {
        // nothing listens on port 9: the request itself fails
        let dead = MisskeyApi::with_scheme(
            "127.0.0.1:9",
            "http",
            HttpClient::new(crate::config::Config::default(), crate::db::memory_pool()),
        );
        assert!(dead.fetch_user_posts("alice", "", 5).await.is_none());

        let server = httpmock::MockServer::start_async().await;
        let host = server.base_url().trim_start_matches("http://").to_string();
        let api = MisskeyApi::with_scheme(
            &host,
            "http",
            HttpClient::new(crate::config::Config::default(), crate::db::memory_pool()),
        );
        server
            .mock_async(|when, then| {
                when.method(httpmock::Method::POST)
                    .path("/api/users/search-by-username-and-host");
                then.status(200)
                    .json_body(serde_json::json!([{"id": "7", "host": null}]));
            })
            .await;
        // a non-200 notes answer
        server
            .mock_async(|when, then| {
                when.method(httpmock::Method::POST).path("/api/users/notes");
                then.status(503);
            })
            .await;
        assert!(api.fetch_user_posts("alice", "", 5).await.is_none());

        // a 200 that is not a list of notes
        server
            .mock_async(|when, then| {
                when.method(httpmock::Method::POST).path("/api/users/notes");
                then.status(200).json_body(serde_json::json!({"oops": 1}));
            })
            .await;
        assert!(api.fetch_user_posts("alice", "", 5).await.is_none());
    }

    #[tokio::test]
    async fn context_urls_come_from_children_and_conversation() {
        let server = httpmock::MockServer::start_async().await;
        let host = server.base_url().trim_start_matches("http://").to_string();
        let api = MisskeyApi::with_scheme(
            &host,
            "http",
            HttpClient::new(crate::config::Config::default(), crate::db::memory_pool()),
        );
        server
            .mock_async(|when, then| {
                when.method(httpmock::Method::POST)
                    .path("/api/notes/children");
                then.status(200)
                    .json_body(serde_json::json!([{"id": "c1"}, {"no": "id"}]));
            })
            .await;
        server
            .mock_async(|when, then| {
                when.method(httpmock::Method::POST)
                    .path("/api/notes/conversation");
                then.status(200)
                    .json_body(serde_json::json!([{"id": "c2"}]));
            })
            .await;
        let urls = api.fetch_context_urls("9", "https://x/notes/9").await;
        assert_eq!(
            urls,
            vec![
                format!("http://{host}/notes/c1"),
                format!("http://{host}/notes/c2")
            ]
        );
    }

    #[tokio::test]
    async fn context_requests_that_fail_are_skipped_or_end_the_fetch() {
        let server = httpmock::MockServer::start_async().await;
        let host = server.base_url().trim_start_matches("http://").to_string();
        let api = MisskeyApi::with_scheme(
            &host,
            "http",
            HttpClient::new(crate::config::Config::default(), crate::db::memory_pool()),
        );
        // children answers 500: skipped, conversation still asked
        server
            .mock_async(|when, then| {
                when.method(httpmock::Method::POST)
                    .path("/api/notes/children");
                then.status(500).up_to_n_times(1);
            })
            .await;
        server
            .mock_async(|when, then| {
                when.method(httpmock::Method::POST)
                    .path("/api/notes/conversation");
                then.status(200)
                    .json_body(serde_json::json!([{"id": "c2"}]))
                    .up_to_n_times(1);
            })
            .await;
        let urls = api.fetch_context_urls("9", "https://x/notes/9").await;
        assert_eq!(urls, vec![format!("http://{host}/notes/c2")]);

        // a 200 whose body is not a list: noted, no urls
        server
            .mock_async(|when, then| {
                when.method(httpmock::Method::POST)
                    .path("/api/notes/children");
                then.status(200).json_body(serde_json::json!("not a list"));
            })
            .await;
        server
            .mock_async(|when, then| {
                when.method(httpmock::Method::POST)
                    .path("/api/notes/conversation");
                then.status(200).json_body(serde_json::json!([]));
            })
            .await;
        assert!(
            api.fetch_context_urls("9", "https://x/notes/9")
                .await
                .is_empty()
        );

        // a request that cannot even be sent: the whole fetch is empty
        let dead = MisskeyApi::with_scheme(
            "127.0.0.1:9",
            "http",
            HttpClient::new(crate::config::Config::default(), crate::db::memory_pool()),
        );
        assert!(
            dead.fetch_context_urls("9", "https://x/notes/9")
                .await
                .is_empty()
        );
    }
}
