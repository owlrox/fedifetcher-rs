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

// one flavor, several ways of naming an account: Mastodon and its kin use
// @name, Pleroma and Akkoma /users/name, and Pixelfed no prefix at all. The
// last would swallow any URL if it were tried against other software, which
// is why nothing asks this of a server it has not identified first.
//
// every way this family addresses a post. Pixelfed is here too: it speaks the
// Mastodon API, so its /p/user/id URLs are this client's to read. Iceshrimp
// is here for the same reason: it speaks the Mastodon API but hands out
// Misskey-style /notes/id URLs, where the id is the status id the API
// addresses the post by.
static POST_PATHS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    [
        r"^https?://[^/]+/@[^/]+/(?P<name>[^/]+)",
        r"^https?://[^/]+/users/[^/]+/statuses/(?P<name>[^/]+)",
        r"^https?://[^/]+/notice/(?P<name>[^/]+)",
        r"^https?://[^/]+/p/[^/]+/(?P<name>[^/]+)",
        r"^https?://[^/]+/notes/(?P<name>[^/]+)",
        // wafrn's post URLs, with the id the API addresses the post by
        r"^https?://[^/]+/fediverse/post/(?P<name>[^/]+)",
    ]
    .iter()
    .map(|p| anchored(p))
    .collect()
});

// Pleroma and Mitra also address a post by an opaque object id, and only the
// redirect it serves says which notice that object is.
static OBJECT_PATH: LazyLock<Regex> = LazyLock::new(|| anchored(r"^https?://[^/]+/objects/[^/]+"));

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
        // wafrn's profile pages
        r"^https?://[^/]+/blog/(?P<name>[^/]+)",
    ]
    .iter()
    .map(|p| anchored(p))
    .collect()
});

/// A status from the Mastodon API, which our own server also speaks.
pub fn parse_post(raw: &Value) -> Option<Post> {
    // every non-boost status carries "reblog": null, which is not a post
    let boosted = raw
        .get("reblog")
        .filter(|reblog| !reblog.is_null())
        .and_then(parse_post);

    // a boost is an empty wrapper: the server gives it no URL of its own, and
    // the post worth reading is the one it carries
    let url = non_empty_str(raw.get("url")).or_else(|| boosted.as_ref().map(|b| b.url.clone()));
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
        in_reply_to_id: raw
            .get("in_reply_to_id")
            .and_then(Value::as_str)
            .map(str::to_string),
        in_reply_to_account_id: raw
            .get("in_reply_to_account_id")
            .and_then(Value::as_str)
            .map(str::to_string),
        reply_count: raw.get("replies_count").and_then(Value::as_i64),
        account: raw.get("account").and_then(parse_user),
        mentions: usable(
            raw.get("mentions")
                .and_then(Value::as_array)
                .map(|mentions| mentions.iter().map(parse_user).collect::<Vec<_>>())
                .unwrap_or_default(),
        ),
    })
}

/// An account, either listed by our server or named by one of its posts.
pub fn parse_user(raw: &Value) -> Option<User> {
    let acct = raw.get("acct").and_then(Value::as_str)?;
    let url = raw.get("url").and_then(Value::as_str)?;
    let note = raw.get("note").and_then(Value::as_str).unwrap_or("");
    Some(User {
        acct: acct.to_string(),
        url: url.to_string(),
        id: raw.get("id").and_then(Value::as_str).map(str::to_string),
        note: note.to_string(),
        // absent means the account never said, which is not the same as no
        indexable: raw
            .get("indexable")
            .and_then(Value::as_bool)
            .unwrap_or(true),
        discoverable: raw
            .get("discoverable")
            .and_then(Value::as_bool)
            .unwrap_or(true),
    })
}

/// One of the lists our own server keeps for the API key owner.
pub fn parse_list(raw: &Value) -> Option<UserList> {
    let list_id = raw.get("id").and_then(Value::as_str)?;
    Some(UserList {
        id: list_id.to_string(),
        title: raw
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
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
                .ok_or_else(|| Error::Fetch("response has no id".to_string()));
        } else if response.status == 404 {
            return Err(Error::Fetch(format!(
                "User {user} was not found on server {}.",
                self.webserver
            )));
        }
        Err(Error::Fetch(format!(
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
        raw.map(|raw| usable(raw.iter().map(parse_post)))
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
    let mut url =
        format!("{base}/api/v1/accounts/{user_id}/statuses?exclude_replies=false&limit={wanted}");
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
            return Err(Error::Fetch(format!("context has no {key}")));
        };
        for toot in toots {
            let Some(url) = toot.get("url").and_then(Value::as_str) else {
                return Err(Error::Fetch("context toot has no url".to_string()));
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
    format!(
        "{}{}{}",
        &token[..5],
        "*".repeat(token.len() - 10),
        &token[token.len() - 5..]
    )
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
    Error::Fetch(format!(
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
/// bookmark, favorite or follow is paged by the id of the record that made
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
                .and_then(timeutil::parse)
                .ok_or_else(|| Error::Fetch("entry has no created_at".to_string()))?;
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

    /// `required_scope` names the permission a 401/403 answer is reported
    /// with, so the log says which scope the access token is missing.
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
            .ok_or_else(|| Error::Fetch("response has no id".to_string()))
    }

    async fn posts(&self, path: &str, stop_at: StopAt, required_scope: &str) -> Result<Vec<Post>> {
        let raw = self.paginated(path, &stop_at, required_scope).await?;
        Ok(usable(raw.iter().map(parse_post)))
    }

    pub async fn bookmarks(&self, limit: i64) -> Result<Vec<Post>> {
        self.posts("/api/v1/bookmarks", StopAt::Count(limit), "read:bookmarks")
            .await
    }

    pub async fn favorites(&self, limit: i64) -> Result<Vec<Post>> {
        self.posts(
            "/api/v1/favourites",
            StopAt::Count(limit),
            "read:favourites",
        )
        .await
    }

    async fn accounts(
        &self,
        path: &str,
        stop_at: StopAt,
        required_scope: &str,
    ) -> Result<Vec<User>> {
        let raw = self.paginated(path, &stop_at, required_scope).await?;
        Ok(usable(raw.iter().map(parse_user)))
    }

    pub async fn follow_requests(&self, limit: i64) -> Result<Vec<User>> {
        self.accounts(
            "/api/v1/follow_requests",
            StopAt::Count(limit),
            "read:follows",
        )
        .await
    }

    pub async fn followers(&self, user_id: &str, limit: i64) -> Result<Vec<User>> {
        self.accounts(
            &format!("/api/v1/accounts/{user_id}/followers"),
            StopAt::Count(limit),
            "read:follows",
        )
        .await
    }

    pub async fn following(&self, user_id: &str, limit: i64) -> Result<Vec<User>> {
        self.accounts(
            &format!("/api/v1/accounts/{user_id}/following"),
            StopAt::Count(limit),
            "read:follows",
        )
        .await
    }

    pub async fn lists(&self) -> Result<Vec<UserList>> {
        let raw = self
            .paginated("/api/v1/lists", &StopAt::All, "read:lists")
            .await?;
        Ok(usable(raw.iter().map(parse_list)))
    }

    pub async fn list_timeline(&self, list_id: &str, limit: i64) -> Result<Vec<Post>> {
        self.posts(
            &format!("/api/v1/timelines/list/{list_id}"),
            StopAt::Count(limit),
            "read:lists",
        )
        .await
    }

    pub async fn list_accounts(&self, list_id: &str, limit: i64) -> Result<Vec<User>> {
        self.accounts(
            &format!("/api/v1/lists/{list_id}/accounts"),
            StopAt::Count(limit),
            "read:lists",
        )
        .await
    }

    /// Accounts appearing in notifications since the given time.
    pub async fn notification_accounts(&self, since: OffsetDateTime) -> Result<Vec<User>> {
        let notifications = self
            .paginated(
                "/api/v1/notifications",
                &StopAt::Since(since),
                "read:notifications",
            )
            .await?;
        let mut accounts: Vec<User> = Vec::new();
        for notification in &notifications {
            let when = notification
                .get("created_at")
                .and_then(Value::as_str)
                .and_then(timeutil::parse);
            let account = notification.get("account").and_then(parse_user);
            if let (Some(when), Some(account)) = (when, account)
                && when >= since
                && !accounts.contains(&account)
            {
                accounts.push(account);
            }
        }
        Ok(accounts)
    }

    /// Get all posts in the user's home timeline.
    pub async fn timeline(&self, limit: i64) -> Result<Vec<Post>> {
        let posts = self
            .posts(
                "/api/v1/timelines/home",
                StopAt::Count(limit),
                "read:statuses",
            )
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
                &format!(
                    "Error getting replies for user {user_id} on server {}",
                    self.server
                ),
                resp.status,
                "read:statuses",
                Some(&self.token_hint),
            ));
        }
        let raw = resp.json()?;
        Ok(match raw.as_array() {
            Some(statuses) => usable(statuses.iter().map(parse_post)),
            None => Vec::new(),
        })
    }

    /// Ask our server to fetch a remote post, so it appears locally.
    pub async fn resolve(&self, url: &str) -> bool {
        let search_url = format!("{}/api/v2/search?q={url}&resolve=true&limit=1", self.base);

        let resp = match self.http.get(&search_url, None).await {
            Ok(resp) => resp,
            Err(ex) => {
                error!(
                    "Error adding url {search_url} to server {}. Exception: {ex}",
                    self.server
                );
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
        parse_post(&json!({
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
    fn parse_post_reads_a_status() {
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
        let post = parse_post(&raw).unwrap();
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
    fn parse_post_reads_a_boost_through_its_wrapper() {
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
        let post = parse_post(&raw).unwrap();
        assert!(post.is_boost());
        assert_eq!(post.original().url, "https://remote.example/@alice/1");
        assert_eq!(post.url, "https://remote.example/@alice/1");
        // visibility comes from the wrapper, not the post it carries
        assert!(post.is_public);
    }

    #[test]
    fn parse_post_treats_a_null_reblog_as_no_boost() {
        // servers send "reblog": null on every non-boost status; reading it
        // as a post would skip every status as unusable
        let raw = json!({
            "id": "1",
            "url": "https://remote.example/@alice/1",
            "created_at": "2026-10-22T01:23:45Z",
            "reblog": null,
        });
        let post = parse_post(&raw).unwrap();
        assert!(!post.is_boost());
    }

    #[test]
    fn parse_post_rejects_a_status_without_url_or_date() {
        assert!(parse_post(&json!({"id": "1"})).is_none());
        assert!(parse_post(&json!({"url": "https://x.example/@a/1"})).is_none());
    }

    #[test]
    fn parse_user_defaults_the_opt_out_fields() {
        let user = parse_user(&json!({"acct": "a", "url": "https://x.example/@a"})).unwrap();
        assert!(user.indexable);
        assert!(user.discoverable);
        assert_eq!(user.note, "");
        assert!(parse_user(&json!({"acct": "a"})).is_none());
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
            api.post_id_from("https://example.com/users/alice/statuses/456")
                .await,
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
        // Iceshrimp addresses a post this way, with the status id the
        // Mastodon API knows it by
        assert_eq!(
            api.post_id_from("https://example.com/notes/asloypdjeaoyizzm")
                .await,
            Some("asloypdjeaoyizzm".to_string())
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

    /// A Mastodon client pointed at the mock server.
    fn mock_api(host: &str) -> MastodonApi {
        MastodonApi::with_scheme(host, "http", dummy_http())
    }

    fn response(status: u16, body: &str) -> crate::http::Response {
        use axum::http::{HeaderMap, HeaderValue};
        let mut headers = HeaderMap::new();
        if !body.is_empty() {
            headers.insert("content-type", HeaderValue::from_static("application/json"));
        }
        crate::http::Response {
            status,
            headers,
            text: body.to_string(),
        }
    }

    #[test]
    fn the_error_report_names_the_scope_and_the_token() {
        // 401: the token itself is wrong
        let err = report_mastodon_error("boom", 401, "read:statuses", Some("tok**********en"));
        let message = err.to_string();
        assert!(message.contains("access token is incorrect"), "{message}");
        assert!(message.contains("tok**********en"), "{message}");

        // 403 with a scope: the scope is named
        let err = report_mastodon_error("boom", 403, "read:bookmarks", None);
        assert!(
            err.to_string().contains("Add the read:bookmarks scope"),
            "{}",
            err
        );

        // 403 without a scope: generic advice
        let err = report_mastodon_error("boom", 403, "", None);
        assert!(
            err.to_string().contains("enabled the required scope(s)"),
            "{}",
            err
        );

        // any other code: no advice line
        let err = report_mastodon_error("boom", 500, "read:statuses", None);
        assert!(!err.to_string().contains('\n'), "{}", err);
    }

    #[test]
    fn the_timeline_line_lists_the_uris_or_just_the_count() {
        let post = |uri: &str| Post {
            url: uri.to_string(),
            uri: uri.to_string(),
            created_at: crate::timeutil::now(),
            is_public: true,
            reblog: None,
            in_reply_to_id: None,
            in_reply_to_account_id: None,
            reply_count: None,
            account: None,
            mentions: Vec::new(),
        };
        assert_eq!(
            timeline_found_line(&[post("https://a/1"), post("https://a/2")]),
            "Found 2 toots in timeline: https://a/1, https://a/2"
        );
        assert_eq!(timeline_found_line(&[]), "Found 0 toots in timeline");
    }

    #[test]
    fn context_urls_need_both_halves_and_every_toot_to_carry_a_url() {
        let ok = response(
            200,
            r#"{"ancestors": [{"url": "https://a/1"}], "descendants": [{"url": "https://a/2"}]}"#,
        );
        assert_eq!(
            context_urls(&ok).unwrap(),
            vec!["https://a/1", "https://a/2"]
        );

        // no ancestors key: the context is unusable
        let half = response(200, r#"{"descendants": []}"#);
        assert!(context_urls(&half).is_err());

        // a toot without a url
        let urlless = response(200, r#"{"ancestors": [{}], "descendants": []}"#);
        assert!(context_urls(&urlless).is_err());

        // a body that is not JSON at all
        let garbage = response(200, "<not json");
        assert!(context_urls(&garbage).is_err());
    }

    #[test]
    fn wants_more_stops_on_count_since_and_all() {
        use time::macros::datetime;
        let entries = vec![serde_json::json!({"created_at": "2026-10-01T00:00:00Z"})];
        assert!(wants_more(&entries, &StopAt::All).unwrap());
        assert!(!wants_more(&entries, &StopAt::Count(1)).unwrap());
        assert!(wants_more(&entries, &StopAt::Count(5)).unwrap());
        // a post newer than the cutoff keeps the pages coming
        assert!(wants_more(&entries, &StopAt::Since(datetime!(2025-01-01 00:00:00 UTC))).unwrap());
        // one older stops it
        assert!(!wants_more(&entries, &StopAt::Since(datetime!(2027-01-01 00:00:00 UTC))).unwrap());
        // an entry with no readable date is an error, not a silent stop
        let undated = vec![serde_json::json!({"nope": 1})];
        assert!(wants_more(&undated, &StopAt::Since(datetime!(2025-01-01 00:00:00 UTC))).is_err());
    }

    #[tokio::test]
    async fn a_user_lookup_failure_is_reported_with_its_status() {
        let server = httpmock::MockServer::start_async().await;
        let host = server.base_url().trim_start_matches("http://").to_string();
        let api = mock_api(&host);
        // 404: the user is not there
        server
            .mock_async(|when, then| {
                when.method(httpmock::Method::GET)
                    .path("/api/v1/accounts/lookup");
                then.status(404).up_to_n_times(1);
            })
            .await;
        let err = api.user_id("ghost").await.unwrap_err();
        assert!(err.to_string().contains("was not found"), "{err}");

        // 500: the server stumbled
        server
            .mock_async(|when, then| {
                when.method(httpmock::Method::GET)
                    .path("/api/v1/accounts/lookup");
                then.status(500).up_to_n_times(1);
            })
            .await;
        let err = api.user_id("ghost").await.unwrap_err();
        assert!(err.to_string().contains("Status code: 500"), "{err}");

        // 200 without an id in the answer
        server
            .mock_async(|when, then| {
                when.method(httpmock::Method::GET)
                    .path("/api/v1/accounts/lookup");
                then.status(200).json_body(serde_json::json!({}));
            })
            .await;
        let err = api.user_id("ghost").await.unwrap_err();
        assert!(err.to_string().contains("no id"), "{err}");
    }

    #[tokio::test]
    async fn post_fetches_survive_every_failure_shape() {
        let server = httpmock::MockServer::start_async().await;
        let host = server.base_url().trim_start_matches("http://").to_string();
        let api = mock_api(&host);
        // the lookup answers fine...
        server
            .mock_async(|when, then| {
                when.method(httpmock::Method::GET)
                    .path("/api/v1/accounts/lookup");
                then.status(200)
                    .json_body(serde_json::json!({"id": "7"}))
                    .up_to_n_times(3);
            })
            .await;
        // ...but the statuses call fails in every shape: a non-200
        server
            .mock_async(|when, then| {
                when.method(httpmock::Method::GET)
                    .path("/api/v1/accounts/7/statuses");
                then.status(503).up_to_n_times(1);
            })
            .await;
        assert!(api.fetch_user_posts("alice", "", 5).await.is_none());
        // ...a 200 that is not a list
        server
            .mock_async(|when, then| {
                when.method(httpmock::Method::GET)
                    .path("/api/v1/accounts/7/statuses");
                then.status(200)
                    .json_body(serde_json::json!({}))
                    .up_to_n_times(1);
            })
            .await;
        assert!(api.fetch_user_posts("alice", "", 5).await.is_none());

        // ...and a lookup that fails outright
        server
            .mock_async(|when, then| {
                when.method(httpmock::Method::GET)
                    .path("/api/v1/accounts/lookup");
                then.status(500).up_to_n_times(1);
            })
            .await;
        assert!(api.fetch_user_posts("alice", "", 5).await.is_none());

        // a client whose server is not there at all
        let dead = MastodonApi::with_scheme("127.0.0.1:9", "http", dummy_http());
        assert!(dead.fetch_user_posts("alice", "", 5).await.is_none());
    }

    #[tokio::test]
    async fn a_full_page_is_followed_by_a_max_id_page() {
        let server = httpmock::MockServer::start_async().await;
        let host = server.base_url().trim_start_matches("http://").to_string();
        let api = mock_api(&host);
        server
            .mock_async(|when, then| {
                when.method(httpmock::Method::GET)
                    .path("/api/v1/accounts/lookup");
                then.status(200).json_body(serde_json::json!({"id": "7"}));
            })
            .await;
        let status = |id: &str| serde_json::json!({"id": id, "url": format!("https://a/{id}"), "uri": format!("https://a/{id}"), "created_at": "2026-10-01T00:00:00Z", "visibility": "public"});
        // a full first page (the pager only continues on a filled page)
        let first_page: Vec<Value> = (1..=40).map(|n| status(&format!("s{n:03}"))).collect();
        let first = server
            .mock_async(|when, then| {
                when.method(httpmock::Method::GET)
                    .path("/api/v1/accounts/7/statuses");
                then.status(200).json_body(first_page).up_to_n_times(1);
            })
            .await;
        let last_of_first = "s040";
        let second = server
            .mock_async(move |when, then| {
                when.method(httpmock::Method::GET)
                    .path("/api/v1/accounts/7/statuses")
                    .query_param("max_id", last_of_first);
                then.status(200).json_body(serde_json::json!([]));
            })
            .await;
        let posts = api.fetch_user_posts("alice", "", 50).await.unwrap();
        assert_eq!(posts.len(), 40, "the empty second page adds nothing");
        assert_eq!(first.calls(), 1);
        assert_eq!(
            second.calls(),
            1,
            "the pager resumes from the first page's last status id"
        );
    }

    #[tokio::test]
    async fn a_page_whose_last_status_has_no_id_stops_the_paging() {
        let server = httpmock::MockServer::start_async().await;
        let host = server.base_url().trim_start_matches("http://").to_string();
        let api = mock_api(&host);
        server
            .mock_async(|when, then| {
                when.method(httpmock::Method::GET)
                    .path("/api/v1/accounts/lookup");
                then.status(200).json_body(serde_json::json!({"id": "7"}));
            })
            .await;
        // a filled page whose last status has no id: the pager cannot know
        // where to resume
        let mut page: Vec<Value> = (1..=39)
            .map(|n| {
                serde_json::json!({"id": format!("s{n:03}"), "url": format!("https://a/s{n:03}"), "uri": format!("https://a/s{n:03}"), "created_at": "2026-10-01T00:00:00Z", "visibility": "public"})
            })
            .collect();
        page.push(
            serde_json::json!({"created_at": "2026-10-02T00:00:00Z", "visibility": "public"}),
        );
        server
            .mock_async(|when, then| {
                when.method(httpmock::Method::GET)
                    .path("/api/v1/accounts/7/statuses");
                then.status(200).json_body(page);
            })
            .await;
        let posts = api.fetch_user_posts("alice", "", 50).await.unwrap();
        assert_eq!(posts.len(), 39, "the idless status is unusable");
    }

    #[tokio::test]
    async fn context_fetch_failures_are_quiet_and_empty() {
        let server = httpmock::MockServer::start_async().await;
        let host = server.base_url().trim_start_matches("http://").to_string();
        let api = mock_api(&host);
        // a non-200 context
        server
            .mock_async(|when, then| {
                when.method(httpmock::Method::GET)
                    .path("/api/v1/statuses/9/context");
                then.status(500);
            })
            .await;
        assert!(api.fetch_context_urls("9", "https://a/9").await.is_empty());

        // a context whose body is not JSON
        server
            .mock_async(|when, then| {
                when.method(httpmock::Method::GET)
                    .path("/api/v1/statuses/9/context");
                then.status(200).body("<not json").up_to_n_times(1);
            })
            .await;
        assert!(api.fetch_context_urls("9", "https://a/9").await.is_empty());

        // a server that is not there at all
        let dead = MastodonApi::with_scheme("127.0.0.1:9", "http", dummy_http());
        assert!(dead.fetch_context_urls("9", "https://a/9").await.is_empty());
    }

    #[tokio::test]
    async fn resolve_reports_each_answer_shape() {
        let server = httpmock::MockServer::start_async().await;
        let host = server.base_url().trim_start_matches("http://").to_string();
        let home = HomeServer::with_scheme(&host, "http", "secret-token", dummy_http());
        // 200: resolved
        server
            .mock_async(|when, then| {
                when.method(httpmock::Method::GET).path("/api/v2/search");
                then.status(200)
                    .json_body(serde_json::json!({}))
                    .up_to_n_times(1);
            })
            .await;
        assert!(home.resolve("https://a/1").await);
        // 403: the scope is named in the log, still false
        server
            .mock_async(|when, then| {
                when.method(httpmock::Method::GET).path("/api/v2/search");
                then.status(403).up_to_n_times(1);
            })
            .await;
        assert!(!home.resolve("https://a/2").await);
        // any other status: false
        server
            .mock_async(|when, then| {
                when.method(httpmock::Method::GET).path("/api/v2/search");
                then.status(500).up_to_n_times(1);
            })
            .await;
        assert!(!home.resolve("https://a/3").await);
        // a server that is not there: false
        let dead = HomeServer::with_scheme("127.0.0.1:9", "http", "secret-token", dummy_http());
        assert!(!dead.resolve("https://a/4").await);
    }

    #[tokio::test]
    async fn account_statuses_reports_a_failure_and_tolerates_a_non_list() {
        let server = httpmock::MockServer::start_async().await;
        let host = server.base_url().trim_start_matches("http://").to_string();
        let home = HomeServer::with_scheme(&host, "http", "secret-token", dummy_http());
        server
            .mock_async(|when, then| {
                when.method(httpmock::Method::GET)
                    .path("/api/v1/accounts/7/statuses");
                then.status(403).up_to_n_times(1);
            })
            .await;
        let err = home.account_statuses("7").await.unwrap_err();
        assert!(
            err.to_string().contains("Error getting replies for user 7"),
            "{err}"
        );
        // a 200 that is not a list: empty, not an error
        server
            .mock_async(|when, then| {
                when.method(httpmock::Method::GET)
                    .path("/api/v1/accounts/7/statuses");
                then.status(200)
                    .json_body(serde_json::json!({}))
                    .up_to_n_times(1);
            })
            .await;
        assert!(home.account_statuses("7").await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn active_user_ids_skip_unreadable_entries_and_report_failures() {
        let server = httpmock::MockServer::start_async().await;
        let host = server.base_url().trim_start_matches("http://").to_string();
        let home = HomeServer::with_scheme(&host, "http", "secret-token", dummy_http());
        server
            .mock_async(|when, then| {
                when.method(httpmock::Method::GET)
                    .path("/api/v1/admin/accounts");
                then.status(403).up_to_n_times(1);
            })
            .await;
        let err = home.active_user_ids(1.0).await.unwrap_err();
        assert!(err.to_string().contains("Error getting user IDs"), "{err}");

        // an hour ago is inside the activity window the interval computes
        let recent =
            crate::timeutil::fmt_state_ts(crate::timeutil::now() - time::Duration::hours(1));
        server
            .mock_async(|when, then| {
                when.method(httpmock::Method::GET)
                    .path("/api/v1/admin/accounts");
                then.status(200).json_body(serde_json::json!([
                    // no last_status_at: skipped
                    {"id": "1"},
                    // an unreadable last_status_at: skipped
                    {"id": "2", "account": {"last_status_at": "not a date"}},
                    // active, but no username: counted without a log line
                    {"id": "3", "account": {"last_status_at": recent}},
                    // active with a username: counted and logged
                    {"id": "4", "username": "alice", "account": {"last_status_at": recent}},
                ]));
            })
            .await;
        let ids = home.active_user_ids(1.0).await.unwrap();
        assert_eq!(ids, vec!["3".to_string(), "4".to_string()]);
    }

    #[tokio::test]
    async fn a_paginated_collection_reports_a_failed_page() {
        let server = httpmock::MockServer::start_async().await;
        let host = server.base_url().trim_start_matches("http://").to_string();
        let home = HomeServer::with_scheme(&host, "http", "secret-token", dummy_http());
        server
            .mock_async(|when, then| {
                when.method(httpmock::Method::GET).path("/api/v1/lists");
                then.status(403).up_to_n_times(1);
            })
            .await;
        let err = home.lists().await.unwrap_err();
        assert!(
            err.to_string().contains("Add the read:lists scope"),
            "{err}"
        );
    }
}
