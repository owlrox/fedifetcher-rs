//! HTTP on FediFetcher's behalf: one client that respects robots.txt (unless
//! configured otherwise), the instance blocklist, and rate limits.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use reqwest::header::{HeaderMap, HeaderValue, LINK};
use reqwest::Method;
use serde_json::Value;
use time::OffsetDateTime;
use tokio::time::sleep;
use tracing::{debug, info, warn};

use crate::config::Config;
use crate::error::{Error, Result};
use crate::robots::RobotFileParser;
use crate::timeutil;
use crate::urls::{netloc_of, scheme_of};
use crate::VERSION;

const ROBOTS_CACHE_PREFIX: &str = "robots-";

/// The xxh128 of a string, as 32 lowercase hex digits.
///
/// XXH128 is XXH3-128, which is what Python's `xxhash.xxh128` computes; the
/// digest is written high half first, matching both `xxhsum` and Python's
/// `hexdigest()`, so robots.txt cache files are named identically by both
/// implementations.
pub fn xxh128_hex(input: &str) -> String {
    format!("{:032x}", xxhash_rust::xxh3::xxh3_128(input.as_bytes()))
}

/// A response with the body already read: FediFetcher never streams.
pub struct Response {
    pub status: u16,
    pub headers: HeaderMap,
    pub text: String,
}

impl Response {
    pub fn json(&self) -> Result<Value> {
        serde_json::from_str(&self.text)
            .map_err(|ex| Error(format!("Error parsing response as JSON: {ex}")))
    }

    /// The `Link` response header, as a map of `rel` to URL, the way
    /// `requests` exposes it as `response.links`.
    pub fn links(&self) -> HashMap<String, String> {
        let mut links = HashMap::new();
        let Some(value) = self.headers.get(LINK).and_then(|v| v.to_str().ok()) else {
            return links;
        };
        for part in split_link_header(value) {
            let Some(url) = part.strip_prefix('<').and_then(|rest| rest.split('>').next()) else {
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
}

impl HttpClient {
    pub fn new(config: Config) -> Self {
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
        }
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
            HeaderValue::from_str(&format!("Bearer {token}")).unwrap_or(HeaderValue::from_static("")),
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
                    self.config.state_dir.clone(),
                    self.config.instance_blocklist.clone(),
                    self.config.respect_robots_txt,
                    self.ignoring_robots(),
                ))
            })
            .clone()
    }

    pub fn user_agent(&self) -> String {
        if self.config.hide_server_in_user_agent {
            format!("FediFetcher/{VERSION} (https://go.thms.uk/ff)")
        } else {
            format!(
                "FediFetcher/{VERSION}; +{} (https://go.thms.uk/ff)",
                self.config.server
            )
        }
    }

    /// Make a get request while providing our user agent, and respecting rate
    /// limits.
    pub async fn get(
        &self,
        url: &str,
        timeout: Option<Duration>,
        max_tries: i64,
        backoff: f64,
    ) -> Result<Response> {
        self.request(Method::GET, url, None, timeout, max_tries, backoff, true)
            .await
    }

    /// Make a post request while providing our user agent, and respecting
    /// rate limits.
    pub async fn post(
        &self,
        url: &str,
        json: &Value,
        timeout: Option<Duration>,
        max_tries: i64,
        backoff: f64,
    ) -> Result<Response> {
        self.request(Method::POST, url, Some(json), timeout, max_tries, backoff, true)
            .await
    }

    /// Get the URL the given URL redirects to.
    pub async fn get_redirect_url(&self, url: &str) -> Option<String> {
        let response = match self
            .request(Method::HEAD, url, None, None, 5, 0.5, false)
            .await
        {
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

    async fn request(
        &self,
        method: Method,
        url: &str,
        json: Option<&Value>,
        timeout: Option<Duration>,
        mut max_tries: i64,
        mut backoff: f64,
        allow_redirects: bool,
    ) -> Result<Response> {
        let mut headers = self.headers.clone().unwrap_or_default();
        headers
            .entry(reqwest::header::USER_AGENT)
            .or_insert_with(|| HeaderValue::from_str(&self.user_agent()).unwrap_or(HeaderValue::from_static("")));

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
                    return Err(Error(format!("Querying {url} prohibited by robots.txt")));
                }
                Err(ex) => {
                    // the configured blocklist; the error names the instance
                    info!("Not fetching {url}: {ex}");
                    return Err(ex);
                }
            }
        }

        let timeout = timeout.unwrap_or_else(|| Duration::from_secs(self.config.http_timeout.max(0) as u64));

        let client = if allow_redirects {
            &self.client
        } else {
            &self.no_redirect_client
        };

        loop {
            let mut request = client
                .request(method.clone(), url)
                .headers(headers.clone())
                .timeout(timeout);
            if let Some(json) = json {
                request = request.json(json);
            }
            let response = request
                .send()
                .await
                .map_err(|ex| Error(format!("Error requesting {url}: {ex}")))?;

            let status = response.status().as_u16();
            if status != 429 {
                let headers = response.headers().clone();
                let text = response
                    .text()
                    .await
                    .map_err(|ex| Error(format!("Error reading response from {url}: {ex}")))?;
                return Ok(Response {
                    status,
                    headers,
                    text,
                });
            }

            if max_tries <= 0 {
                return Err(Error(format!(
                    "Maximum number of retries exceeded for rate limited request {url}"
                )));
            }

            let wait = retry_delay(&response_headers(&response), url, backoff);
            sleep(wait).await;
            max_tries -= 1;
            backoff *= 4.0;
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
    {
        if let Some(reset_at) = timeutil::parse(value) {
            wait = (reset_at - now).whole_microseconds() as f64 / 1_000_000.0 + 1.0;
            reset = reset_at;
        }
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

/// Remembers each host's robots.txt, in memory and on disk.
pub struct RobotsCache {
    state_dir: PathBuf,
    blocklist: Vec<String>,
    enabled: bool,
    fetcher: HttpClient,
    cache: Mutex<HashMap<String, RobotsEntry>>,
}

impl RobotsCache {
    fn new(
        state_dir: PathBuf,
        blocklist: Vec<String>,
        enabled: bool,
        fetcher: HttpClient,
    ) -> Self {
        RobotsCache {
            state_dir,
            blocklist,
            enabled,
            fetcher,
            cache: Mutex::new(HashMap::new()),
        }
    }

    pub fn cache_path(&self, robots_url: &str) -> PathBuf {
        self.state_dir
            .join(format!("{ROBOTS_CACHE_PREFIX}{}.txt", xxh128_hex(robots_url)))
    }

    fn cached(&self, robots_url: &str) -> Option<RobotsEntry> {
        if let Some(entry) = self.cache.lock().unwrap().get(robots_url) {
            return Some(match entry {
                RobotsEntry::Text(text) => RobotsEntry::Text(text.clone()),
                RobotsEntry::Verdict(verdict) => RobotsEntry::Verdict(*verdict),
            });
        }

        let path = self.cache_path(robots_url);
        if path.exists() {
            debug!("Getting robots.txt file from cache for {robots_url}.");
            if let Ok(text) = std::fs::read_to_string(&path) {
                self.cache
                    .lock()
                    .unwrap()
                    .insert(robots_url.to_string(), RobotsEntry::Text(text.clone()));
                return Some(RobotsEntry::Text(text));
            }
        }
        None
    }

    async fn fetch(&self, robots_url: &str) -> RobotsEntry {
        if let Some(entry) = self.cached(robots_url) {
            return entry;
        }

        // We are getting the robots.txt manually from here, because otherwise
        // we can't change the User Agent
        let entry = match self
            .fetcher
            .get(robots_url, Some(Duration::from_secs(2)), 5, 0.5)
            .await
        {
            Ok(response) if response.status == 401 || response.status == 403 => {
                RobotsEntry::Verdict(false)
            }
            Ok(response) => {
                let text = response.text;
                std::fs::create_dir_all(&self.state_dir).ok();
                std::fs::write(self.cache_path(robots_url), &text).ok();
                RobotsEntry::Text(text)
            }
            Err(_) => RobotsEntry::Verdict(true),
        };

        self.cache
            .lock()
            .unwrap()
            .insert(robots_url.to_string(), match &entry {
                RobotsEntry::Text(text) => RobotsEntry::Text(text.clone()),
                RobotsEntry::Verdict(verdict) => RobotsEntry::Verdict(*verdict),
            });
        entry
    }

    pub async fn can_fetch(&self, user_agent: &str, url: &str) -> Result<bool> {
        if !self.enabled {
            return Ok(true);
        }
        let netloc = netloc_of(url);
        if self.blocklist.iter().any(|blocked| *blocked == netloc) {
            // Never connect to these locations
            return Err(Error(format!(
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

    /// Delete cached robots.txt files old enough to be worth re-fetching.
    pub fn discard_stale_files(&self, max_age: time::Duration) -> usize {
        if !self.enabled {
            return 0;
        }
        let Ok(entries) = std::fs::read_dir(&self.state_dir) else {
            return 0;
        };

        let cutoff = timeutil::now() - max_age;
        let mut discarded = 0;
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            if name.starts_with(ROBOTS_CACHE_PREFIX) && path.is_file() {
                let Ok(metadata) = entry.metadata() else {
                    continue;
                };
                let Ok(modified) = metadata.modified() else {
                    continue;
                };
                let modified = OffsetDateTime::from(modified);
                if modified < cutoff {
                    debug!("Removing cached robots.txt file {}", path.display());
                    std::fs::remove_file(&path).ok();
                    discarded += 1;
                }
            }
        }
        discarded
    }
}

/// Add query parameters to a callback URL, replacing any that already exist.
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
            b'%'
                if i + 2 < bytes.len()
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
mod tests {
    use super::*;

    #[test]
    fn xxh128_matches_pythons_hexdigest() {
        // Ground truth from python-xxhash: xxhash.xxh128(data).hexdigest(),
        // which the Python original uses for its robots.txt cache filenames.
        // (xxhash.xxh128 is xxh3_128 under another name.)
        let hundred = "a".repeat(100);
        let urls = "https://example.com/robots.txt\n".repeat(4);
        let thousand = "x".repeat(1024);
        let vectors: &[(&str, &str)] = &[
            ("", "99aa06d3014798d86001c324468d497f"),
            ("abc", "06b05ab6733a618578af5f94892f3950"),
            (
                "https://mastodon.social/robots.txt",
                "3b90ad97973ad4c93c3a346a52b68940",
            ),
            (hundred.as_str(), "7d09cdccbc6bead248fb02115fe96a8a"),
            (urls.as_str(), "4053371a0ffe8cb3e5ae10a1aded53f8"),
            (thousand.as_str(), "445dcf97d7880d62fc56a7ce85b3164a"),
        ];
        for (input, expected) in vectors {
            assert_eq!(&xxh128_hex(input), expected, "input of {} bytes", input.len());
        }
    }

    #[test]
    fn callback_url_replaces_and_adds_params() {
        assert_eq!(
            build_callback_url(
                "https://watch.example/ping?rid=old&keep=1",
                &[("rid".to_string(), "new".to_string()), ("msg".to_string(), "hi there".to_string())]
            ),
            "https://watch.example/ping?rid=new&keep=1&msg=hi+there"
        );
        assert_eq!(
            build_callback_url("https://watch.example/ping", &[("a".to_string(), "1".to_string())]),
            "https://watch.example/ping?a=1"
        );
        // blank values are kept
        assert_eq!(
            build_callback_url("https://watch.example/ping?empty=", &[("a".to_string(), "1".to_string())]),
            "https://watch.example/ping?empty=&a=1"
        );
    }

    #[test]
    fn link_header_parses() {
        let mut headers = HeaderMap::new();
        headers.insert(
            LINK,
            HeaderValue::from_static(
                r#"<https://home.example/api/v1/bookmarks?max_id=42>; rel="next", <https://home.example/api/v1/bookmarks?min_id=99>; rel="prev""#,
            ),
        );
        let response = Response {
            status: 200,
            headers,
            text: String::new(),
        };
        let links = response.links();
        assert_eq!(
            links.get("next"),
            Some(&"https://home.example/api/v1/bookmarks?max_id=42".to_string())
        );
        assert_eq!(
            links.get("prev"),
            Some(&"https://home.example/api/v1/bookmarks?min_id=99".to_string())
        );
    }
}
