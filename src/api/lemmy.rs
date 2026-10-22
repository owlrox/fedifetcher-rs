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

static COMMUNITY_PATH: LazyLock<Regex> = LazyLock::new(|| anchored(r"^https://[^/]+/c/"));
static USER_PATH: LazyLock<Regex> = LazyLock::new(|| anchored(r"^https://[^/]+/u/"));

static PROFILE_PATHS: LazyLock<Vec<Regex>> =
    LazyLock::new(|| vec![anchored(r"^https://[^/]+/(?:u|c)/(?P<name>[^/]+)")]);
static POST_PATHS: LazyLock<Vec<Regex>> =
    LazyLock::new(|| vec![anchored(r"^https://[^/]+/(?:comment|post)/(?P<name>[^/]+)")]);

/// A Lemmy post or comment, which names itself by its ActivityPub id.
pub fn to_post(raw: &Value) -> Option<Post> {
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
    webserver: String,
    http: HttpClient,
}

impl LemmyApi {
    pub fn new(webserver: &str, http: HttpClient) -> Self {
        LemmyApi {
            webserver: webserver.to_string(),
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
        let webserver = self.webserver.clone();
        let path = path.to_string();
        let describe = describe.to_string();
        let raw = in_pages(
            |wanted, gathered| {
                let http = http.clone();
                let webserver = webserver.clone();
                let path = path.clone();
                let describe = describe.clone();
                let page = gathered.len() as i64 / PAGE_SIZE + 1;
                async move {
                    let url = format!(
                        "https://{webserver}/api/v3/{path}&sort=New&limit={wanted}&page={page}"
                    );
                    let response = match http.get(&url, None, 5, 0.5).await {
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
        raw.map(|raw| usable(raw.iter().map(to_post)))
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
        let url = format!("https://{}/api/v3/comment?id={comment_id}", self.webserver);
        let resp = match self.http.get(&url, None, 5, 0.5).await {
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
        let url = format!("https://{}/api/v3/post?id={post_id}", self.webserver);
        let resp = match self.http.get(&url, None, 5, 0.5).await {
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
                            error!("Error parsing post {post_id} from {post_url}. Exception: KeyError");
                        }
                    }
                }
                Err(ex) => {
                    error!("Error parsing post {post_id} from {post_url}. Exception: {ex}");
                }
            }
        }

        let url = format!(
            "https://{}/api/v3/comment/list?post_id={post_id}&sort=New&limit=50",
            self.webserver
        );
        let resp = match self.http.get(&url, None, 5, 0.5).await {
            Ok(resp) => resp,
            Err(ex) => {
                error!("Error getting comments for post {post_id} from {post_url}. Exception: {ex}");
                return Vec::new();
            }
        };

        if resp.status == 200 {
            match resp.json() {
                Ok(res) => {
                    let parsed = res.get("comments").and_then(Value::as_array).map(|comments| {
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
                            error!("Error parsing comments for post {post_url}. Exception: KeyError");
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
    fn to_post_reads_a_lemmy_post() {
        let raw = json!({
            "id": 42,
            "ap_id": "https://lemmy.example/post/42",
            "published": "2026-10-22T01:23:45.123Z",
        });
        let post = to_post(&raw).unwrap();
        assert_eq!(post.url, "https://lemmy.example/post/42");
        assert_eq!(post.uri, "https://lemmy.example/post/42");
        assert!(post.is_public);
    }

    #[test]
    fn to_post_rejects_a_post_without_ap_id_or_date() {
        assert!(to_post(&json!({"id": 1})).is_none());
        assert!(to_post(&json!({"ap_id": "https://lemmy.example/post/1"})).is_none());
    }

    #[test]
    fn paths_read_names() {
        let api = LemmyApi::new(
            "lemmy.example",
            HttpClient::new(crate::config::Config::default()),
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
}
