//! HTTP on FediFetcher's behalf: one client that respects robots.txt (unless
//! configured otherwise), the instance blocklist, and rate limits.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use reqwest::Method;
use reqwest::header::{HeaderMap, HeaderValue, LINK};
use serde_json::Value;
use sqlx::SqlitePool;
use tokio::time::sleep;
use tracing::{debug, info, warn};

use crate::VERSION;
use crate::config::Config;
use crate::db;
use crate::error::{Error, Result};
use crate::robots::RobotFileParser;
use crate::timeutil;
use crate::urls::{netloc_of, scheme_of};

/// The body a request carries: nothing, JSON, or a pre-encoded urlencoded
/// form.
enum Body<'a> {
    None,
    Json(&'a Value),
    Form(String),
}

/// A response with the body already read: FediFetcher never streams.
pub struct Response {
    pub status: u16,
    pub headers: HeaderMap,
    pub text: String,
}

impl Response {
    pub fn json(&self) -> Result<Value> {
        serde_json::from_str(&self.text).map_err(Error::Json)
    }

    /// The `Link` response header, as a map of `rel` to URL, the way
    /// `requests` exposes it as `response.links`.
    pub fn links(&self) -> HashMap<String, String> {
        let mut links = HashMap::new();
        let Some(value) = self.headers.get(LINK).and_then(|v| v.to_str().ok()) else {
            return links;
        };
        for part in split_link_header(value) {
            let Some(url) = part
                .strip_prefix('<')
                .and_then(|rest| rest.split('>').next())
            else {
                continue;
            };
            for param in part.split('>').nth(1).unwrap_or_default().split(';') {
                let param = param.trim();
                let Some(rel) = param.strip_prefix("rel") else {
                    continue;
                };
                let rel = rel.trim_start_matches([' ', '=']).trim();
                let rel = rel.trim_matches('"');
                links.insert(rel.to_string(), url.to_string());
            }
        }
        links
    }
}

/// Split a `Link` header on commas that are not inside angle brackets.
fn split_link_header(value: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut in_angles = false;
    for c in value.chars() {
        match c {
            '<' => in_angles = true,
            '>' => in_angles = false,
            ',' if !in_angles => {
                parts.push(current.trim().to_string());
                current = String::new();
                continue;
            }
            _ => {}
        }
        current.push(c);
    }
    if !current.trim().is_empty() {
        parts.push(current.trim().to_string());
    }
    parts
}

/// Makes requests on FediFetcher's behalf, respecting robots.txt and rate
/// limits.
#[derive(Clone)]
pub struct HttpClient {
    config: Config,
    client: reqwest::Client,
    /// The same client, but never following redirects: resolving a post URL
    /// to where it really lives needs to see the redirect itself.
    no_redirect_client: reqwest::Client,
    robots: Arc<OnceLock<Arc<RobotsCache>>>,
    headers: Option<HeaderMap>,
    ignore_robots: bool,
    /// The scheme the server lookups build their URLs with: production
    /// always asks real servers over https, tests point this at a local
    /// http server.
    scheme: &'static str,
    /// The database the robots.txt cache lives in.
    db: SqlitePool,
}

impl HttpClient {
    pub fn new(config: Config, db: SqlitePool) -> Self {
        HttpClient::with_scheme(config, db, "https")
    }

    /// A client whose server lookups go to `scheme://`. Production always
    /// uses `new` (https); tests point this at a local http server.
    pub fn with_scheme(config: Config, db: SqlitePool, scheme: &'static str) -> Self {
        // a client keeps connections alive between the many requests we make
        // to the same handful of hosts
        HttpClient {
            config,
            client: reqwest::Client::new(),
            no_redirect_client: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .unwrap_or_else(|_| reqwest::Client::new()),
            robots: Arc::new(OnceLock::new()),
            headers: None,
            ignore_robots: false,
            scheme,
            db,
        }
    }

    /// The scheme this client's server lookups are built with.
    pub fn scheme(&self) -> &'static str {
        self.scheme
    }

    /// A client that will ignore robots.txt.
    pub fn ignoring_robots(&self) -> HttpClient {
        let mut derived = self.clone();
        derived.ignore_robots = true;
        derived
    }

    /// A client for our own server: it sends our token, and robots.txt does
    /// not apply.
    pub fn authenticated(&self, token: &str) -> HttpClient {
        let mut headers = self.headers.clone().unwrap_or_default();
        headers.insert(
            reqwest::header::AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {token}"))
                .unwrap_or(HeaderValue::from_static("")),
        );
        let mut derived = self.clone();
        derived.headers = Some(headers);
        derived.ignore_robots = true;
        derived
    }

    pub fn robots(&self) -> Arc<RobotsCache> {
        self.robots
            .get_or_init(|| {
                Arc::new(RobotsCache::new(
                    self.db.clone(),
                    self.config.instance_blocklist.clone(),
                    self.config.respect_robots_txt,
                    self.ignoring_robots(),
                ))
            })
            .clone()
    }

    pub fn user_agent(&self) -> String {
        user_agent_string(&self.config)
    }

    /// Make a get request while providing our user agent, and respecting rate
    /// limits. A 429 is retried along the configured cooldown series: one wait
    /// per entry of `rate_limit_cooldown`, in order, until the series runs out.
    pub async fn get(&self, url: &str, timeout: Option<Duration>) -> Result<Response> {
        self.send(Method::GET, url, Body::None, timeout, true).await
    }

    /// Make a post request while providing our user agent, and respecting
    /// rate limits, on the same cooldown series as `get`.
    pub async fn post(
        &self,
        url: &str,
        json: &Value,
        timeout: Option<Duration>,
    ) -> Result<Response> {
        self.send(Method::POST, url, Body::Json(json), timeout, true)
            .await
    }

    /// POST a urlencoded form — OAuth's registration and token exchange —
    /// while providing our user agent, and respecting rate limits, on the
    /// same cooldown series as `get`.
    pub async fn post_form(
        &self,
        url: &str,
        form: &[(String, String)],
        timeout: Option<Duration>,
    ) -> Result<Response> {
        let body = url::form_urlencoded::Serializer::new(String::new())
            .extend_pairs(form)
            .finish();
        self.send(Method::POST, url, Body::Form(body), timeout, true)
            .await
    }

    /// A long-lived GET whose body the caller reads incrementally: the
    /// server's event stream. No timeout — the connection is meant to sit
    /// idle between events — and redirects are not followed, so a redirect
    /// to another host cannot silently lose the access token. The status is
    /// the caller's to check.
    pub async fn get_stream(&self, url: &str) -> Result<reqwest::Response> {
        let mut headers = self.headers.clone().unwrap_or_default();
        headers
            .entry(reqwest::header::USER_AGENT)
            .or_insert_with(|| {
                HeaderValue::from_str(&self.user_agent()).unwrap_or(HeaderValue::from_static(""))
            });
        headers.insert(
            reqwest::header::ACCEPT,
            HeaderValue::from_static("text/event-stream"),
        );
        self.no_redirect_client
            .get(url)
            .headers(headers)
            .send()
            .await
            .map_err(|ex| Error::Http {
                url: url.to_string(),
                source: ex,
            })
    }

    /// Get the URL the given URL redirects to.
    pub async fn get_redirect_url(&self, url: &str) -> Option<String> {
        let response = match self.send(Method::HEAD, url, Body::None, None, false).await {
            Ok(response) => response,
            Err(ex) => {
                tracing::error!("Error getting redirect URL for URL {url}. Exception: {ex}");
                return None;
            }
        };

        if response.status == 200 {
            return Some(url.to_string());
        } else if response.status == 302 {
            debug!("Discovered redirect for URL {url}");
            return response
                .headers
                .get(reqwest::header::LOCATION)
                .and_then(|v| v.to_str().ok())
                .map(str::to_string);
        }
        tracing::error!(
            "Error getting redirect URL for URL {url}. Status code: {}",
            response.status
        );
        None
    }

    async fn send(
        &self,
        method: Method,
        url: &str,
        body: Body<'_>,
        timeout: Option<Duration>,
        allow_redirects: bool,
    ) -> Result<Response> {
        let mut headers = self.headers.clone().unwrap_or_default();
        headers
            .entry(reqwest::header::USER_AGENT)
            .or_insert_with(|| {
                HeaderValue::from_str(&self.user_agent()).unwrap_or(HeaderValue::from_static(""))
            });

        let user_agent = headers
            .get(reqwest::header::USER_AGENT)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_string();
        if !self.ignore_robots && self.config.respect_robots_txt {
            match Box::pin(self.robots().can_fetch(&user_agent, url)).await {
                Ok(true) => {}
                Ok(false) => {
                    info!("Not fetching {url}: its robots.txt disallows it");
                    return Err(Error::Fetch(format!(
                        "Querying {url} prohibited by robots.txt"
                    )));
                }
                Err(ex) => {
                    // the configured blocklist; the error names the instance
                    info!("Not fetching {url}: {ex}");
                    return Err(ex);
                }
            }
        }

        let timeout =
            timeout.unwrap_or_else(|| Duration::from_secs(self.config.http_timeout.max(0) as u64));

        let client = if allow_redirects {
            &self.client
        } else {
            &self.no_redirect_client
        };

        // the configured cooldown series: retry step i waits cooldown[i]
        // seconds (or until the server's reset header says), and when the
        // series is exhausted the request fails
        let cooldown = &self.config.rate_limit_cooldown;
        let mut step = 0usize;

        loop {
            let mut request = client
                .request(method.clone(), url)
                .headers(headers.clone())
                .timeout(timeout);
            match &body {
                Body::None => {}
                Body::Json(json) => request = request.json(json),
                Body::Form(form) => {
                    request = request
                        .header(
                            reqwest::header::CONTENT_TYPE,
                            "application/x-www-form-urlencoded",
                        )
                        .body(form.clone());
                }
            }
            let response = request.send().await.map_err(|ex| Error::Http {
                url: url.to_string(),
                source: ex,
            })?;

            let status = response.status().as_u16();
            if status != 429 {
                let headers = response.headers().clone();
                let text = response.text().await.map_err(|ex| Error::HttpRead {
                    url: url.to_string(),
                    source: ex,
                })?;
                return Ok(Response {
                    status,
                    headers,
                    text,
                });
            }

            let Some(&backoff) = cooldown.get(step) else {
                return Err(Error::Fetch(format!(
                    "Maximum number of retries exceeded for rate limited request {url}"
                )));
            };

            let wait = retry_delay(&response_headers(&response), url, backoff);
            sleep(wait).await;
            step += 1;
        }
    }
}

fn response_headers(response: &reqwest::Response) -> HeaderMap {
    response.headers().clone()
}

/// How long to wait before retrying a rate limited request: until the server
/// says the limit resets, or the backoff, whichever the header provides.
fn retry_delay(headers: &HeaderMap, url: &str, backoff: f64) -> Duration {
    let now = timeutil::now();
    let mut wait = backoff;
    let mut reset = now + time::Duration::seconds_f64(backoff);
    if let Some(value) = headers
        .get("x-ratelimit-reset")
        .and_then(|v| v.to_str().ok())
        && let Some(reset_at) = timeutil::parse(value)
    {
        wait = (reset_at - now).whole_microseconds() as f64 / 1_000_000.0 + 1.0;
        reset = reset_at;
    }
    let wait = wait.max(0.0);
    warn!(
        "Rate Limit hit requesting {url}. Waiting {wait} sec to retry at {}",
        timeutil::fmt_state_ts(reset)
    );
    Duration::from_secs_f64(wait)
}

enum RobotsEntry {
    /// The text of a robots.txt.
    Text(String),
    /// A verdict reached without one: false when the host refused to serve
    /// it, true when it could not be fetched at all and we go ahead
    /// regardless.
    Verdict(bool),
}

/// Remembers each host's robots.txt, in memory and in the database.
pub struct RobotsCache {
    db: SqlitePool,
    blocklist: Vec<String>,
    enabled: bool,
    fetcher: HttpClient,
    cache: Mutex<HashMap<String, RobotsEntry>>,
}

impl RobotsCache {
    fn new(db: SqlitePool, blocklist: Vec<String>, enabled: bool, fetcher: HttpClient) -> Self {
        RobotsCache {
            db,
            blocklist,
            enabled,
            fetcher,
            cache: Mutex::new(HashMap::new()),
        }
    }

    async fn cached(&self, robots_url: &str) -> Option<RobotsEntry> {
        if let Some(entry) = self.cache.lock().unwrap().get(robots_url) {
            return Some(match entry {
                RobotsEntry::Text(text) => RobotsEntry::Text(text.clone()),
                RobotsEntry::Verdict(verdict) => RobotsEntry::Verdict(*verdict),
            });
        }

        match db::robots_cached(&self.db, robots_url).await {
            Ok(Some(text)) => {
                debug!("Getting robots.txt from the database cache for {robots_url}.");
                self.cache
                    .lock()
                    .unwrap()
                    .insert(robots_url.to_string(), RobotsEntry::Text(text.clone()));
                Some(RobotsEntry::Text(text))
            }
            Ok(None) => None,
            Err(ex) => {
                // an unreadable cache is an empty one: fetch the file fresh
                warn!("Could not read the robots.txt cache for {robots_url}: {ex}");
                None
            }
        }
    }

    async fn fetch(&self, robots_url: &str) -> RobotsEntry {
        if let Some(entry) = self.cached(robots_url).await {
            return entry;
        }

        // We are getting the robots.txt manually from here, because otherwise
        // we can't change the User Agent
        let entry = match self
            .fetcher
            .get(robots_url, Some(Duration::from_secs(2)))
            .await
        {
            Ok(response) if response.status == 401 || response.status == 403 => {
                RobotsEntry::Verdict(false)
            }
            Ok(response) => {
                let text = response.text;
                if let Err(ex) = db::robots_store(&self.db, robots_url, &text).await {
                    warn!("Could not cache the robots.txt for {robots_url}: {ex}");
                }
                RobotsEntry::Text(text)
            }
            Err(ex) => {
                warn!(
                    "Could not fetch the robots.txt for {robots_url}: {ex} — \
                     treating every path as allowed until it can be fetched"
                );
                RobotsEntry::Verdict(true)
            }
        };

        self.cache.lock().unwrap().insert(
            robots_url.to_string(),
            match &entry {
                RobotsEntry::Text(text) => RobotsEntry::Text(text.clone()),
                RobotsEntry::Verdict(verdict) => RobotsEntry::Verdict(*verdict),
            },
        );
        entry
    }

    pub async fn can_fetch(&self, user_agent: &str, url: &str) -> Result<bool> {
        if !self.enabled {
            return Ok(true);
        }
        let netloc = netloc_of(url);
        if self.blocklist.contains(&netloc) {
            // Never connect to these locations
            return Err(Error::Fetch(format!(
                "Connecting to {netloc} is prohibited by the configured blocklist"
            )));
        }

        let scheme = scheme_of(url).unwrap_or("https");
        let robots_url = format!("{scheme}://{netloc}/robots.txt");
        match self.fetch(&robots_url).await {
            RobotsEntry::Verdict(verdict) => Ok(verdict),
            RobotsEntry::Text(text) => {
                let mut parser = RobotFileParser::new();
                parser.parse(text.lines().map(str::to_string));
                Ok(parser.can_fetch(user_agent, url))
            }
        }
    }

    /// Delete cached robots.txt entries old enough to be worth re-fetching.
    pub async fn discard_stale(&self, max_age: time::Duration) -> usize {
        if !self.enabled {
            return 0;
        }
        match db::robots_discard_stale(&self.db, max_age).await {
            Ok(discarded) => discarded as usize,
            Err(ex) => {
                warn!("Could not discard stale robots.txt cache entries: {ex}");
                0
            }
        }
    }
}

/// Add query parameters to a callback URL, replacing any that already exist.
/// The User-Agent sent to other servers: FediFetcher and the user's server,
/// unless the server's name is hidden — or the whole thing is replaced by
/// the config's override, when one is set.
fn user_agent_string(config: &Config) -> String {
    if let Some(agent) = &config.user_agent_override {
        // a blank agent is refused or ignored by servers, so a blank
        // override is unset: the default agent is sent, never empty
        let agent = agent.trim();
        if !agent.is_empty() {
            return agent.to_string();
        }
    }
    if config.hide_server_in_user_agent {
        format!("FediFetcher/{VERSION} (https://go.thms.uk/ff)")
    } else {
        format!(
            "FediFetcher/{VERSION}; +{} (https://go.thms.uk/ff)",
            config.server
        )
    }
}

pub fn build_callback_url(url: &str, params: &[(String, String)]) -> String {
    let (base, query) = match url.split_once('?') {
        Some((base, query)) => (base, query),
        None => (url, ""),
    };

    // parse_qs(keep_blank_values=True): pairs in order of first appearance
    let mut pairs: Vec<(String, String)> = Vec::new();
    for pair in query.split('&').filter(|pair| !pair.is_empty()) {
        let (key, value) = match pair.split_once('=') {
            Some((key, value)) => (key, value),
            None => (pair, ""),
        };
        pairs.push((decode_component(key), decode_component(value)));
    }
    for (key, value) in params {
        if let Some(existing) = pairs.iter_mut().find(|(k, _)| k == key) {
            existing.1 = value.clone();
        } else {
            pairs.push((key.clone(), value.clone()));
        }
    }

    let query = pairs
        .iter()
        .map(|(key, value)| format!("{}={}", encode_component(key), encode_component(value)))
        .collect::<Vec<_>>()
        .join("&");
    if query.is_empty() {
        base.to_string()
    } else {
        format!("{base}?{query}")
    }
}

/// `urllib.parse.quote_plus`: everything but letters, digits and `_.-~` is
/// percent-encoded, and spaces become `+`.
fn encode_component(value: &str) -> String {
    let mut out = String::new();
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'_' | b'.' | b'-' | b'~' => {
                out.push(byte as char)
            }
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// `urllib.parse.unquote_plus`.
fn decode_component(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 2 < bytes.len()
                && bytes[i + 1].is_ascii_hexdigit()
                && bytes[i + 2].is_ascii_hexdigit() =>
            {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("00");
                out.push(u8::from_str_radix(hex, 16).unwrap_or(b'?'));
                i += 3;
            }
            byte => {
                out.push(byte);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests;
