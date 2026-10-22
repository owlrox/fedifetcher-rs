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
    LazyLock::new(|| vec![anchored(r"^https://[^/]+/@(?P<name>[^/]+)")]);
static POST_PATHS: LazyLock<Vec<Regex>> =
    LazyLock::new(|| vec![anchored(r"^https://[^/]+/notes/(?P<name>[^/]+)")]);

/// A Misskey note, which only has a URL of its own when it is a copy.
pub fn to_post(raw: &Value, webserver: &str) -> Option<Post> {
    let note_id = raw.get("id").and_then(Value::as_str);
    let created_at = raw.get("createdAt").and_then(parse_date);
    let (Some(note_id), Some(created_at)) = (note_id, created_at) else {
        unusable("misskey", raw.get("id").unwrap_or(&Value::Null));
        return None;
    };

    let url =
        non_empty_str(raw.get("url")).unwrap_or_else(|| format!("https://{webserver}/notes/{note_id}"));
    let renote = raw
        .get("renote")
        .filter(|renote| !renote.is_null())
        .and_then(|renote| to_post(renote, webserver));

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
        in_reply_to_id: raw.get("replyId").and_then(Value::as_str).map(str::to_string),
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
    http: HttpClient,
}

impl MisskeyApi {
    pub fn new(webserver: &str, http: HttpClient) -> Self {
        MisskeyApi {
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
        _profile_url: &str,
        limit: i64,
    ) -> Option<Vec<Post>> {
        let user_id = self.find_user_id(username).await?;

        let http = self.http.clone();
        let webserver = self.webserver.clone();
        let username = username.to_string();
        let raw = in_pages(
            |wanted, gathered| {
                let http = http.clone();
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
                            notes(&http, &webserver, &user_id, &username, wanted, Some(&id)).await
                        }
                        Some(None) => {
                            error!("Error getting posts for user {username}: note has no id");
                            None
                        }
                        None => notes(&http, &webserver, &user_id, &username, wanted, None).await,
                    }
                }
            },
            limit,
            PAGE_SIZE,
        )
        .await;
        let webserver = self.webserver.clone();
        raw.map(|raw| usable(raw.iter().map(|note| to_post(note, &webserver))))
    }

    async fn find_user_id(&self, username: &str) -> Option<String> {
        // query user info via search api
        // we could filter by host but there's no way to limit that to just the
        // main host on firefish currently; on misskey it works if you supply
        // '.' as the host but firefish does not
        let url = format!(
            "https://{}/api/users/search-by-username-and-host",
            self.webserver
        );
        let resp = match self
            .http
            .post(&url, &serde_json::json!({ "username": username }), None, 5, 0.5)
            .await
        {
            Ok(resp) => resp,
            Err(ex) => {
                error!("Error finding user {username} from {}. Exception: {ex}", self.webserver);
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
                error!("Error finding user {username} from {}. Exception: {ex}", self.webserver);
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
            let url = format!("https://{}/api/notes/{endpoint}", self.webserver);
            let resp = match self.http.post(&url, &params, None, 5, 0.5).await {
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
                                urls.push(format!("https://{}/notes/{id}", self.webserver));
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

    let url = format!("https://{webserver}/api/users/notes");
    let resp = match http.post(&url, &params, None, 5, 0.5).await {
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
    fn to_post_reads_a_note() {
        let raw = json!({
            "id": "9abc",
            "createdAt": "2026-10-22T01:23:45.123Z",
            "visibility": "home",
            "replyId": "8def",
            "repliesCount": 2,
        });
        let post = to_post(&raw, "misskey.example").unwrap();
        assert_eq!(post.url, "https://misskey.example/notes/9abc");
        assert_eq!(post.uri, "https://misskey.example/notes/9abc");
        assert!(post.is_public);
        assert_eq!(post.in_reply_to_id.as_deref(), Some("8def"));
        assert_eq!(post.reply_count, Some(2));
    }

    #[test]
    fn to_post_prefers_the_notes_own_url_and_reads_renotes() {
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
        let post = to_post(&raw, "misskey.example").unwrap();
        assert_eq!(post.url, "https://elsewhere.example/notes/1");
        assert!(!post.is_public);
        assert!(post.is_boost());
        assert_eq!(post.original().url, "https://misskey.example/notes/2");
        assert!(post.original().is_public);
    }

    #[test]
    fn to_post_rejects_a_note_without_id_or_date() {
        assert!(to_post(&json!({"createdAt": "2026-10-22T01:23:45Z"}), "misskey.example").is_none());
        assert!(to_post(&json!({"id": "1"}), "misskey.example").is_none());
    }

    #[test]
    fn paths_read_names() {
        let api = MisskeyApi::new(
            "misskey.example",
            HttpClient::new(crate::config::Config::default()),
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
}
