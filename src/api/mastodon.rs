//! The Mastodon API: Mastodon, Pleroma, Pixelfed and friends, plus the client
//! for our own server, which speaks this API with an access token.

use std::sync::LazyLock;
use std::time::Duration;

use regex::Regex;
use serde_json::Value;
use time::OffsetDateTime;
use tracing::{debug, error, info};

use crate::api::paging::in_pages;
use crate::error::{Error, Result};
use crate::http::HttpClient;
use crate::lists::UserList;
use crate::posts::Post;
use crate::timeutil;
use crate::translate::{anchored, first_name_in, non_empty_str, parse_date, unusable, usable};
use crate::users::User;

/// A status anyone is allowed to read.
pub const PUBLIC: &[&str] = &["public", "unlisted"];

/// The most statuses this API will hand out at once.
pub const PAGE_SIZE: i64 = 40;

// one flavour, several ways of naming an account: Mastodon and its kin use
// @name, Pleroma and Akkoma /users/name, and Pixelfed no prefix at all. The
// last would swallow any URL if it were tried against other software, which
// is why nothing asks this of a server it has not identified first.
//
// every way this family addresses a post. Pixelfed is here too: it speaks the
// Mastodon API, so its /p/user/id URLs are this client's to read.
static POST_PATHS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    [
        r"^https?://[^/]+/@[^/]+/(?P<name>[^/]+)",
        r"^https?://[^/]+/users/[^/]+/statuses/(?P<name>[^/]+)",
        r"^https?://[^/]+/notice/(?P<name>[^/]+)",
        r"^https?://[^/]+/p/[^/]+/(?P<name>[^/]+)",
    ]
    .iter()
    .map(|p| anchored(p))
    .collect()
});

// Pleroma and Mitra also address a post by an opaque object id, and only the
// redirect it serves says which notice that object is.
static OBJECT_PATH: LazyLock<Regex> =
    LazyLock::new(|| anchored(r"^https?://[^/]+/objects/[^/]+"));

static REDIRECTED_OBJECT_PATH: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    [
        // Pleroma uses /notice/ as relative
        r"/notice/(?P<name>[^/]+)",
        // Mitra uses an absolute url containing /post/
        r"^https?://[^/]+/post/(?P<name>[^/]+)",
    ]
    .iter()
    .map(|p| anchored(p))
    .collect()
});

static PROFILE_PATHS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    [
        r"^https?://[^/]+/@(?P<name>[^/]+)",
        r"^https?://[^/]+/users/(?P<name>[^/]+)",
        r"^https?://[^/]+/(?P<name>[^/]+)/?$",
    ]
    .iter()
    .map(|p| anchored(p))
    .collect()
});

/// A status from the Mastodon API, which our own server also speaks.
pub fn to_post(raw: &Value) -> Option<Post> {
    // every non-boost status carries "reblog": null, which is not a post
    let boosted = raw
        .get("reblog")
        .filter(|reblog| !reblog.is_null())
        .and_then(to_post);

    // a boost is an empty wrapper: the server gives it no URL of its own, and
    // the post worth reading is the one it carries
    let url = non_empty_str(raw.get("url"))
        .or_else(|| boosted.as_ref().map(|b| b.url.clone()));
    let created_at = raw
        .get("created_at")
        .and_then(parse_date)
        .or_else(|| boosted.as_ref().map(|b| b.created_at));
    let (Some(url), Some(created_at)) = (url, created_at) else {
        unusable("mastodon", raw.get("uri").unwrap_or(&Value::Null));
        return None;
    };

    Some(Post {
        uri: non_empty_str(raw.get("uri")).unwrap_or_else(|| url.clone()),
        url,
        created_at,
        is_public: raw
            .get("visibility")
            .and_then(Value::as_str)
            .map(|v| PUBLIC.contains(&v))
            .unwrap_or(false),
        reblog: boosted.map(Box::new),
        in_reply_to_id: raw.get("in_reply_to_id").and_then(Value::as_str).map(str::to_string),
        in_reply_to_account_id: raw
            .get("in_reply_to_account_id")
            .and_then(Value::as_str)
            .map(str::to_string),
        reply_count: raw.get("replies_count").and_then(Value::as_i64),
        account: raw.get("account").and_then(to_user),
        mentions: usable(
            raw.get("mentions")
                .and_then(Value::as_array)
                .map(|mentions| mentions.iter().map(to_user).collect::<Vec<_>>())
                .unwrap_or_default(),
        ),
    })
}

/// An account, either listed by our server or named by one of its posts.
pub fn to_user(raw: &Value) -> Option<User> {
    let acct = raw.get("acct").and_then(Value::as_str)?;
    let url = raw.get("url").and_then(Value::as_str)?;
    let note = raw.get("note").and_then(Value::as_str).unwrap_or("");
    Some(User {
        acct: acct.to_string(),
        url: url.to_string(),
        id: raw.get("id").and_then(Value::as_str).map(str::to_string),
        note: note.to_string(),
        // absent means the account never said, which is not the same as no
        indexable: raw.get("indexable").and_then(Value::as_bool).unwrap_or(true),
        discoverable: raw.get("discoverable").and_then(Value::as_bool).unwrap_or(true),
    })
}

/// One of the lists our own server keeps for the API key owner.
pub fn to_list(raw: &Value) -> Option<UserList> {
    let list_id = raw.get("id").and_then(Value::as_str)?;
    Some(UserList {
        id: list_id.to_string(),
        title: raw.get("title").and_then(Value::as_str).unwrap_or("").to_string(),
    })
}

/// Servers implementing the Mastodon API: Mastodon, Pleroma, Pixelfed and
/// friends.
#[derive(Clone)]
pub struct MastodonApi {
    webserver: String,
    /// The API's base URL, scheme included: `https://host` in production.
    base: String,
    http: HttpClient,
}

impl MastodonApi {
    pub fn new(webserver: &str, http: HttpClient) -> Self {
        Self::with_scheme(webserver, "https", http)
    }

    /// A client for the API at `scheme://webserver`. Production always uses
    /// `new` (https); tests point this at a local http server.
    pub fn with_scheme(webserver: &str, scheme: &str, http: HttpClient) -> Self {
        MastodonApi {
            webserver: webserver.to_string(),
            base: format!("{scheme}://{webserver}"),
            http,
        }
    }

    /// Look up the numeric id the API addresses an account by.
    pub async fn user_id(&self, user: &str) -> Result<String> {
        let url = format!("{}/api/v1/accounts/lookup?acct={user}", self.base);
        let response = self.http.get(&url, None).await?;

        if response.status == 200 {
            return response
                .json()?
                .get("id")
                .and_then(Value::as_str)
                .map(str::to_string)
                .ok_or_else(|| Error("response has no id".to_string()));
        } else if response.status == 404 {
            return Err(Error(format!(
                "User {user} was not found on server {}.",
                self.webserver
            )));
        }
        Err(Error(format!(
            "Error getting URL {url}. Status code: {}",
            response.status
        )))
    }

    pub fn username_from(&self, profile_url: &str) -> Option<String> {
        first_name_in(&PROFILE_PATHS, profile_url)
    }

    pub async fn post_id_from(&self, post_url: &str) -> Option<String> {
        if OBJECT_PATH.is_match(post_url) {
            let redirect = self.http.get_redirect_url(post_url).await?;
            return first_name_in(&REDIRECTED_OBJECT_PATH, &redirect);
        }
        first_name_in(&POST_PATHS, post_url)
    }

    pub async fn fetch_user_posts(
        &self,
        username: &str,
        _profile_url: &str,
        limit: i64,
    ) -> Option<Vec<Post>> {
        let user_id = match self.user_id(username).await {
            Ok(user_id) => user_id,
            Err(ex) => {
                error!("Error getting user ID for user {username}: {ex}");
                return None;
            }
        };

        let base = self.base.clone();
        let http = self.http.clone();
        let username = username.to_string();
        let raw = in_pages(
            |wanted, gathered| {
                let http = http.clone();
                let base = base.clone();
                let username = username.clone();
                let user_id = user_id.clone();
                // everything older than the oldest status we already have
                let max_id = gathered
                    .last()
                    .map(|last| last.get("id").and_then(Value::as_str).map(str::to_string));
                async move {
                    match max_id {
                        Some(Some(id)) => {
                            statuses(&http, &base, &user_id, &username, wanted, Some(&id)).await
                        }
                        Some(None) => {
                            error!("Error getting posts for user {username}: status has no id");
                            None
                        }
                        None => statuses(&http, &base, &user_id, &username, wanted, None).await,
                    }
                }
            },
            limit,
            PAGE_SIZE,
        )
        .await;
        raw.map(|raw| usable(raw.iter().map(to_post)))
    }

    pub async fn fetch_context_urls(&self, post_id: &str, post_url: &str) -> Vec<String> {
        let url = format!("{}/api/v1/statuses/{post_id}/context", self.base);
        let resp = match self.http.get(&url, None).await {
            Ok(resp) => resp,
            Err(ex) => {
                error!("Error getting context for toot {post_url}. Exception: {ex}");
                return Vec::new();
            }
        };

        if resp.status == 200 {
            match context_urls(&resp) {
                Ok(urls) => {
                    debug!("Got context for toot {post_url}");
                    return urls;
                }
                Err(ex) => {
                    error!("Error parsing context for toot {post_url}. Exception: {ex}");
                    return Vec::new();
                }
            }
        }

        error!(
            "Error getting context for toot {post_url}. Status code: {}",
            resp.status
        );
        Vec::new()
    }
}

async fn statuses(
    http: &HttpClient,
    base: &str,
    user_id: &str,
    username: &str,
    wanted: i64,
    max_id: Option<&str>,
) -> Option<Vec<Value>> {
    let mut url = format!(
        "{base}/api/v1/accounts/{user_id}/statuses?exclude_replies=false&limit={wanted}"
    );
    if let Some(max_id) = max_id {
        url.push_str(&format!("&max_id={max_id}"));
    }

    let response = match http.get(&url, None).await {
        Ok(response) => response,
        Err(ex) => {
            error!("Error getting posts for user {username}: {ex}");
            return None;
        }
    };

    if response.status == 200 {
        match response.json() {
            Ok(Value::Array(items)) => return Some(items),
            _ => return None,
        }
    }

    error!(
        "Error getting posts for user {username}. Error getting URL {url}. Status code: {}",
        response.status
    );
    None
}

fn context_urls(resp: &crate::http::Response) -> Result<Vec<String>> {
    let res = resp.json()?;
    let mut urls = Vec::new();
    for key in ["ancestors", "descendants"] {
        let Some(toots) = res.get(key).and_then(Value::as_array) else {
            return Err(Error(format!("context has no {key}")));
        };
        for toot in toots {
            let Some(url) = toot.get("url").and_then(Value::as_str) else {
                return Err(Error("context toot has no url".to_string()));
            };
            urls.push(url.to_string());
        }
    }
    Ok(urls)
}

/// Enough of a token to tell which one it was, and no more.
pub fn masked(token: &str) -> String {
    if token.len() <= 10 {
        return "*".repeat(token.len());
    }
    format!("{}{}{}", &token[..5], "*".repeat(token.len() - 10), &token[token.len() - 5..])
}

pub fn report_mastodon_error(
    error_message: &str,
    error_code: u16,
    required_scope: &str,
    token_hint: Option<&str>,
) -> Error {
    let subline = match error_code {
        401 => "\nIt looks like your access token is incorrect. Consider generating a new access token, and/or ensure you have copy and pasted the whole token correctly.".to_string(),
        403 => {
            if !required_scope.is_empty() {
                format!("\nAdd the {required_scope} scope to your access token, and regenerate the token.")
            } else {
                "\nMake sure you have enabled the required scope(s) for your token.".to_string()
            }
        }
        _ => String::new(),
    };

    // which token failed matters: several may be configured for the one server
    let with_token = match token_hint {
        Some(hint) => format!(" with token {hint}"),
        None => String::new(),
    };
    Error(format!(
        "{error_message}{with_token}. Status code: {error_code} {subline}"
    ))
}

/// Either how many entries are wanted, or the oldest creation date worth
/// having, or nothing at all to read unpaginated endpoints.
pub enum StopAt {
    Count(i64),
    Since(OffsetDateTime),
    All,
}

/// Follow a Mastodon collection across pages.
///
/// The header our server sends is the only way through most of these: a
/// bookmark, favourite or follow is paged by the id of the record that made
/// it, which is an internal number the entries themselves never mention.
/// That is why this asks to be pointed at the next page rather than working
/// it out of what it has, as we have to for other people's servers.
pub async fn get_paginated(
    url: &str,
    stop_at: &StopAt,
    timeout: Option<Duration>,
    http: &HttpClient,
    required_scope: &str,
    token_hint: Option<&str>,
) -> Result<Vec<Value>> {
    // what is wanted overall is not what a page will hold, and asking for more
    // than a server will give is not always forgiven: Mastodon quietly trims
    // an oversized limit, but Sharkey turns the request away with a 400
    let mut next_url = match stop_at {
        StopAt::Count(count) => format!("{url}?limit={}", (*count).min(PAGE_SIZE)),
        _ => url.to_string(),
    };

    let mut result: Vec<Value> = Vec::new();
    loop {
        let response = http.get(&next_url, timeout).await?;
        if response.status != 200 {
            return Err(report_mastodon_error(
                &format!("Error getting URL {next_url}"),
                response.status,
                required_scope,
                token_hint,
            ));
        }

        let page = response.json()?;
        let Value::Array(items) = page else {
            return Ok(result);
        };
        if items.is_empty() {
            return Ok(result);
        }

        result.extend(items);
        let links = response.links();
        if !wants_more(&result, stop_at)? || !links.contains_key("next") {
            return Ok(result);
        }
        next_url = links["next"].clone();
    }
}

fn wants_more(result: &[Value], stop_at: &StopAt) -> Result<bool> {
    match stop_at {
        StopAt::All => Ok(true),
        StopAt::Count(count) => Ok(result.len() < *count as usize),
        StopAt::Since(since) => {
            let last = result.last().expect("page was non-empty");
            let created_at = last
                .get("created_at")
                .and_then(Value::as_str)
                .and_then(|text| timeutil::parse(text))
                .ok_or_else(|| Error("entry has no created_at".to_string()))?;
            Ok(created_at >= *since)
        }
    }
}

/// The "Found N toots in timeline" line: the count, and the ids of the toots
/// found, comma separated. An empty timeline gets the bare count.
fn timeline_found_line(posts: &[Post]) -> String {
    if posts.is_empty() {
        return "Found 0 toots in timeline".to_string();
    }
    let ids = posts
        .iter()
        .map(|post| post.uri.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    format!("Found {} toots in timeline: {ids}", posts.len())
}

/// Our own instance, spoken to with one of the configured access tokens.
#[derive(Clone)]
pub struct HomeServer {
    pub server: String,
    /// The API's base URL, scheme included: `https://server` in production.
    base: String,
    http: HttpClient,
    token_hint: String,
    api: MastodonApi,
}

impl HomeServer {
    pub fn new(server: &str, token: &str, http: HttpClient) -> Self {
        Self::with_scheme(server, "https", token, http)
    }

    /// A client for the API at `scheme://server`. Production always uses
    /// `new` (https); tests point this at a local http server.
    pub fn with_scheme(server: &str, scheme: &str, token: &str, http: HttpClient) -> Self {
        let http = http.authenticated(token);
        HomeServer {
            server: server.to_string(),
            base: format!("{scheme}://{server}"),
            token_hint: masked(token),
            api: MastodonApi::with_scheme(server, scheme, http.clone()),
            http,
        }
    }

    async fn paginated(
        &self,
        path: &str,
        stop_at: &StopAt,
        required_scope: &str,
    ) -> Result<Vec<Value>> {
        get_paginated(
            &format!("{}{path}", self.base),
            stop_at,
            None,
            &self.http,
            required_scope,
            Some(&self.token_hint),
        )
        .await
    }

    pub async fn user_id(&self, user: Option<&str>) -> Result<String> {
        if let Some(user) = user {
            return self.api.user_id(user).await;
        }

        let url = format!("{}/api/v1/accounts/verify_credentials", self.base);
        let resp = self.http.get(&url, None).await?;
        if resp.status != 200 {
            return Err(report_mastodon_error(
                &format!("Error verifying credentials on server {}", self.server),
                resp.status,
                "read:accounts",
                Some(&self.token_hint),
            ));
        }
        resp.json()?
            .get("id")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| Error("response has no id".to_string()))
    }

    async fn posts(&self, path: &str, stop_at: StopAt, required_scope: &str) -> Result<Vec<Post>> {
        let raw = self.paginated(path, &stop_at, required_scope).await?;
        Ok(usable(raw.iter().map(to_post)))
    }

    pub async fn bookmarks(&self, limit: i64) -> Result<Vec<Post>> {
        self.posts("/api/v1/bookmarks", StopAt::Count(limit), "").await
    }

    pub async fn favourites(&self, limit: i64) -> Result<Vec<Post>> {
        self.posts("/api/v1/favourites", StopAt::Count(limit), "").await
    }

    async fn accounts(&self, path: &str, stop_at: StopAt) -> Result<Vec<User>> {
        let raw = self.paginated(path, &stop_at, "").await?;
        Ok(usable(raw.iter().map(to_user)))
    }

    pub async fn follow_requests(&self, limit: i64) -> Result<Vec<User>> {
        self.accounts("/api/v1/follow_requests", StopAt::Count(limit)).await
    }

    pub async fn followers(&self, user_id: &str, limit: i64) -> Result<Vec<User>> {
        self.accounts(
            &format!("/api/v1/accounts/{user_id}/followers"),
            StopAt::Count(limit),
        )
        .await
    }

    pub async fn following(&self, user_id: &str, limit: i64) -> Result<Vec<User>> {
        self.accounts(
            &format!("/api/v1/accounts/{user_id}/following"),
            StopAt::Count(limit),
        )
        .await
    }

    pub async fn lists(&self) -> Result<Vec<UserList>> {
        let raw = self.paginated("/api/v1/lists", &StopAt::All, "").await?;
        Ok(usable(raw.iter().map(to_list)))
    }

    pub async fn list_timeline(&self, list_id: &str, limit: i64) -> Result<Vec<Post>> {
        self.posts(&format!("/api/v1/timelines/list/{list_id}"), StopAt::Count(limit), "")
            .await
    }

    pub async fn list_accounts(&self, list_id: &str, limit: i64) -> Result<Vec<User>> {
        self.accounts(&format!("/api/v1/lists/{list_id}/accounts"), StopAt::Count(limit))
            .await
    }

    /// Accounts appearing in notifications since the given time.
    pub async fn notification_accounts(&self, since: OffsetDateTime) -> Result<Vec<User>> {
        let notifications = self
            .paginated("/api/v1/notifications", &StopAt::Since(since), "")
            .await?;
        let mut accounts: Vec<User> = Vec::new();
        for notification in &notifications {
            let when = notification
                .get("created_at")
                .and_then(Value::as_str)
                .and_then(|text| timeutil::parse(text));
            let account = notification.get("account").and_then(to_user);
            if let (Some(when), Some(account)) = (when, account) {
                if when >= since && !accounts.contains(&account) {
                    accounts.push(account);
                }
            }
        }
        Ok(accounts)
    }

    /// Get all posts in the user's home timeline.
    pub async fn timeline(&self, limit: i64) -> Result<Vec<Post>> {
        let posts = self
            .posts("/api/v1/timelines/home", StopAt::Count(limit), "read:statuses")
            .await?;
        info!("{}", timeline_found_line(&posts));
        Ok(posts)
    }

    /// User IDs on our server that have posted in the given time interval.
    pub async fn active_user_ids(&self, reply_interval_hours: f64) -> Result<Vec<String>> {
        let since = timeutil::now_local()
            - time::Duration::seconds_f64((reply_interval_hours / 24.0 + 1.0) * 86_400.0);
        let url = format!("{}/api/v1/admin/accounts", self.base);
        let resp = self.http.get(&url, None).await?;
        if resp.status != 200 {
            return Err(report_mastodon_error(
                &format!("Error getting user IDs on server {}", self.server),
                resp.status,
                "admin:read:accounts",
                Some(&self.token_hint),
            ));
        }

        let mut ids = Vec::new();
        if let Value::Array(users) = resp.json()? {
            for user in users {
                let last_status_at = user
                    .get("account")
                    .and_then(|a| a.get("last_status_at"))
                    .and_then(Value::as_str);
                let Some(last_status_at) = last_status_at else {
                    continue;
                };
                let Some(last_status) = timeutil::parse(last_status_at) else {
                    continue;
                };
                if last_status > since {
                    if let Some(username) = user.get("username").and_then(Value::as_str) {
                        info!("Found active user: {username}");
                    }
                    if let Some(id) = user.get("id").and_then(Value::as_str) {
                        ids.push(id.to_string());
                    }
                }
            }
        }
        Ok(ids)
    }

    /// Recent posts by one of our users, replies included.
    pub async fn account_statuses(&self, user_id: &str) -> Result<Vec<Post>> {
        let url = format!(
            "{}/api/v1/accounts/{user_id}/statuses?exclude_replies=false&limit=40",
            self.base
        );
        let resp = self.http.get(&url, None).await?;

        if resp.status != 200 {
            return Err(report_mastodon_error(
                &format!("Error getting replies for user {user_id} on server {}", self.server),
                resp.status,
                "read:statuses",
                Some(&self.token_hint),
            ));
        }
        let raw = resp.json()?;
        Ok(match raw.as_array() {
            Some(statuses) => usable(statuses.iter().map(to_post)),
            None => Vec::new(),
        })
    }

    /// Ask our server to fetch a remote post, so it appears locally.
    pub async fn resolve(&self, url: &str) -> bool {
        let search_url = format!(
            "{}/api/v2/search?q={url}&resolve=true&limit=1",
            self.base
        );

        let resp = match self.http.get(&search_url, None).await {
            Ok(resp) => resp,
            Err(ex) => {
                error!("Error adding url {search_url} to server {}. Exception: {ex}", self.server);
                return false;
            }
        };

        if resp.status == 200 {
            debug!("Added context url {url}");
            return true;
        } else if resp.status == 403 {
            error!(
                "Error adding url {search_url} to server {}. Status code: {}. Make sure you have the read:search scope enabled for your access token.",
                self.server, resp.status
            );
            return false;
        }
        error!(
            "Error adding url {search_url} to server {}. Status code: {}",
            self.server, resp.status
        );
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use time::macros::datetime;

    #[test]
    fn masked_hides_the_middle_of_a_token() {
        assert_eq!(masked("short"), "*****");
        assert_eq!(masked("1234567890"), "**********");
        assert_eq!(masked("12345678901"), "12345*78901");
        assert_eq!(masked("abcdefghijklmnopqrst"), "abcde**********pqrst");
    }

    fn post_with_uri(uri: &str) -> Post {
        to_post(&json!({
            "id": "1",
            "url": uri,
            "uri": uri,
            "created_at": "2026-10-22T01:23:45.123Z",
            "visibility": "public",
            "reblog": null,
        }))
        .unwrap()
    }

    #[test]
    fn the_timeline_line_lists_the_toot_ids() {
        let posts = vec![
            post_with_uri("https://remote.example/@alice/1"),
            post_with_uri("https://remote.example/@bob/2"),
        ];
        assert_eq!(
            timeline_found_line(&posts),
            "Found 2 toots in timeline: https://remote.example/@alice/1, https://remote.example/@bob/2"
        );
    }

    #[test]
    fn the_timeline_line_without_toots_is_the_bare_count() {
        assert_eq!(timeline_found_line(&[]), "Found 0 toots in timeline");
    }

    #[test]
    fn to_post_reads_a_status() {
        let raw = json!({
            "id": "1",
            "url": "https://remote.example/@alice/1",
            "uri": "https://remote.example/@alice/1",
            "created_at": "2026-10-22T01:23:45.123Z",
            "visibility": "public",
            "in_reply_to_id": "9",
            "in_reply_to_account_id": "8",
            "replies_count": 3,
            "reblog": null,
            "account": {"acct": "alice", "url": "https://remote.example/@alice", "id": "8"},
            "mentions": [{"acct": "bob@remote.example", "url": "https://remote.example/@bob", "id": "7"}],
        });
        let post = to_post(&raw).unwrap();
        assert_eq!(post.url, "https://remote.example/@alice/1");
        assert_eq!(post.created_at, datetime!(2026-10-22 01:23:45.123 UTC));
        assert!(post.is_public);
        assert!(!post.is_boost());
        assert_eq!(post.in_reply_to_id.as_deref(), Some("9"));
        assert_eq!(post.reply_count, Some(3));
        assert_eq!(post.account.as_ref().unwrap().acct, "alice");
        assert_eq!(post.mentions.len(), 1);
        assert!(post.may_have_context());
    }

    #[test]
    fn to_post_reads_a_boost_through_its_wrapper() {
        let raw = json!({
            "id": "2",
            "visibility": "public",
            "reblog": {
                "id": "1",
                "url": "https://remote.example/@alice/1",
                "uri": "https://remote.example/@alice/1",
                "created_at": "2026-10-22T01:23:45Z",
                "visibility": "unlisted",
            },
        });
        let post = to_post(&raw).unwrap();
        assert!(post.is_boost());
        assert_eq!(post.original().url, "https://remote.example/@alice/1");
        assert_eq!(post.url, "https://remote.example/@alice/1");
        // visibility comes from the wrapper, not the post it carries
        assert!(post.is_public);
    }

    #[test]
    fn to_post_treats_a_null_reblog_as_no_boost() {
        // servers send "reblog": null on every non-boost status; reading it
        // as a post would skip every status as unusable
        let raw = json!({
            "id": "1",
            "url": "https://remote.example/@alice/1",
            "created_at": "2026-10-22T01:23:45Z",
            "reblog": null,
        });
        let post = to_post(&raw).unwrap();
        assert!(!post.is_boost());
    }

    #[test]
    fn to_post_rejects_a_status_without_url_or_date() {
        assert!(to_post(&json!({"id": "1"})).is_none());
        assert!(to_post(&json!({"url": "https://x.example/@a/1"})).is_none());
    }

    #[test]
    fn to_user_defaults_the_opt_out_fields() {
        let user = to_user(&json!({"acct": "a", "url": "https://x.example/@a"})).unwrap();
        assert!(user.indexable);
        assert!(user.discoverable);
        assert_eq!(user.note, "");
        assert!(to_user(&json!({"acct": "a"})).is_none());
    }

    #[tokio::test]
    async fn post_id_from_reads_the_mastodon_shapes() {
        let api = MastodonApi::new("example.com", dummy_http());
        assert_eq!(
            api.username_from("https://example.com/@alice"),
            Some("alice".to_string())
        );
        assert_eq!(
            api.username_from("https://example.com/users/bob"),
            Some("bob".to_string())
        );
        assert_eq!(
            api.username_from("https://example.com/charlie"),
            Some("charlie".to_string())
        );
        assert_eq!(api.username_from("https://example.com/"), None);
    }

    #[tokio::test]
    async fn post_id_from_reads_post_shapes_without_requests() {
        let api = MastodonApi::new("example.com", dummy_http());
        assert_eq!(
            api.post_id_from("https://example.com/@alice/123").await,
            Some("123".to_string())
        );
        assert_eq!(
            api.post_id_from("https://example.com/users/alice/statuses/456").await,
            Some("456".to_string())
        );
        assert_eq!(
            api.post_id_from("https://example.com/notice/789").await,
            Some("789".to_string())
        );
        assert_eq!(
            api.post_id_from("https://example.com/p/alice/abc").await,
            Some("abc".to_string())
        );
        assert_eq!(api.post_id_from("https://example.com/other").await, None);
    }

    fn dummy_http() -> HttpClient {
        use crate::config::Config;
        HttpClient::new(
            Config {
                server: "home.example".to_string(),
                access_tokens: vec!["t".to_string()],
                ..Default::default()
            },
            crate::db::memory_pool(),
        )
    }
}
