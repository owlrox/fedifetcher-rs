//! Configuration: one struct, built from defaults and the values stored in
//! the database.
//!
//! The database is the only source of configuration — the web UI edits it
//! there — so there is no config file, no `ff_*` environment variables and
//! no command line flags.

use std::collections::BTreeMap;
use std::path::PathBuf;

use regex::Regex;
use serde_json::Value;

use crate::error::{Error, Result};

const TRUTHY: &[&str] = &["1", "true", "yes", "on"];
const FALSEY: &[&str] = &["0", "false", "no", "off", ""];

/// Where a checkout keeps its state, and has since long before there was a
/// choice.
const LEGACY_STATE_DIR: &str = "artifacts";

/// Where state goes when nobody says otherwise.
///
/// Every checkout has an artifacts/ directory, because artifacts/blank is
/// committed, so this keeps the clone-and-cron installs that FediFetcher is
/// usually deployed as writing exactly where they always have. An installed
/// FediFetcher, run from wherever the user happens to be, gets a home of its
/// own instead of scattering state across working directories.
pub fn default_state_dir() -> PathBuf {
    if std::path::Path::new(LEGACY_STATE_DIR).is_dir() {
        return PathBuf::from(LEGACY_STATE_DIR);
    }
    if let Ok(xdg) = std::env::var("XDG_STATE_HOME")
        && !xdg.is_empty()
    {
        return PathBuf::from(xdg).join("fedifetcher");
    }
    if let Some(home) = std::env::var_os("HOME") {
        return PathBuf::from(home)
            .join(".local")
            .join("state")
            .join("fedifetcher");
    }
    PathBuf::from(".local/state/fedifetcher")
}

/// Everything FediFetcher needs to know to do a run.
///
/// Two options that the Python original hardcoded are configurable here and
/// default to enabled: `respect_robots_txt` and `respect_user_opt_out`.
#[derive(Clone, Debug)]
pub struct Config {
    /// Required: The name of your server (e.g. `mstdn.thms.uk`)
    pub server: String,
    /// Required: access tokens, one per user to run for. Generate at
    /// https://<server>/settings/applications; the token must have
    /// read:search, read:statuses and admin:read:accounts scopes.
    pub access_tokens: Vec<String>,
    /// Fetch remote replies to posts that have received replies from users on
    /// your own instance in this period
    pub reply_interval_in_hours: i64,
    /// Look for replies to posts in the API-Key owner's home timeline, up to
    /// this many posts
    pub home_timeline_length: i64,
    /// Stream the home timeline over a long-lived connection, fetching
    /// context for new posts as they arrive instead of at the next run
    /// (serve mode only, with the first access token)
    pub streaming: bool,
    /// Optional. Use together with max_followings or max_followers to backfill
    /// a specific user's followings/followers. If omitted, we use the account
    /// that owns the access token.
    pub user: String,
    /// Backfill posts for new accounts followed by `user`, at most this many
    pub max_followings: i64,
    /// Backfill posts for new accounts following `user`, at most this many
    pub max_followers: i64,
    /// Backfill posts of the API key owner's pending follow requests, at most
    /// this many
    pub max_follow_requests: i64,
    /// Fetch remote replies to the API key owner's bookmarks, at most this many
    pub max_bookmarks: i64,
    /// Fetch remote replies to the API key owner's favorites, at most this many
    pub max_favorites: i64,
    /// Backfill accounts of anyone appearing in your notifications, during
    /// the last hours
    pub from_notifications: i64,
    /// Fetch missing replies and/or backfill accounts from your lists
    pub from_lists: bool,
    /// How many posts we'll fetch replies for in each list
    pub max_list_length: i64,
    /// How many accounts we'll backfill for in each list
    pub max_list_accounts: i64,
    /// How many posts to fetch from each account we backfill
    pub max_posts_per_account: i64,
    /// Fetch remote replies when backfilling profiles
    pub backfill_with_context: bool,
    /// Backfill any mentioned users when fetching remote replies to timeline posts
    pub backfill_mentioned_users: bool,
    /// Respect robots.txt when fetching from remote servers (default: enabled)
    pub respect_robots_txt: bool,
    /// Skip accounts that have opted out (nobot tag, or not indexable or
    /// discoverable) when backfilling (default: enabled)
    pub respect_user_opt_out: bool,
    /// How long to remember users that you aren't following for, before trying
    /// to backfill them again
    pub remember_users_for_hours: i64,
    /// How long to remember host info for, before checking again
    pub remember_hosts_for_days: i64,
    /// The timeout for any HTTP requests to your own, or other instances
    pub http_timeout: i64,
    /// Seconds to wait between retries when a server rate limits us (HTTP
    /// 429): one entry per retry step, tried in order; when the list runs
    /// out, the request fails
    pub rate_limit_cooldown: Vec<f64>,
    /// Omit your server's name from the User-Agent sent to other servers
    /// (default: disabled)
    pub hide_server_in_user_agent: bool,
    /// Instances that FediFetcher should never try to connect to
    pub instance_blocklist: Vec<String>,
    /// Directory to store persistent files and possibly lock file
    pub state_dir: PathBuf,
    /// Location of the lock file
    pub lock_file: Option<PathBuf>,
    /// The lock timeout in hours
    pub lock_hours: i64,
    /// A url that will be pinged when processing is starting
    pub on_start: Option<String>,
    /// A url that will be pinged when processing has completed
    pub on_done: Option<String>,
    /// A url that will be pinged when processing has failed
    pub on_fail: Option<String>,
    /// Severity of events to log (DEBUG|INFO|WARNING|ERROR|CRITICAL)
    pub log_level: String,
    /// Specify the log format
    pub log_format: String,
    /// Log a line for every page load of the web UI (default: disabled)
    pub log_page_loads: bool,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            server: String::new(),
            access_tokens: Vec::new(),
            reply_interval_in_hours: 0,
            home_timeline_length: 0,
            streaming: false,
            user: String::new(),
            max_followings: 0,
            max_followers: 0,
            max_follow_requests: 0,
            max_bookmarks: 0,
            max_favorites: 0,
            from_notifications: 0,
            from_lists: false,
            max_list_length: 100,
            max_list_accounts: 10,
            max_posts_per_account: 40,
            backfill_with_context: true,
            backfill_mentioned_users: true,
            respect_robots_txt: true,
            respect_user_opt_out: true,
            remember_users_for_hours: 24 * 7,
            remember_hosts_for_days: 30,
            http_timeout: 5,
            rate_limit_cooldown: vec![0.5, 2.0, 8.0, 32.0, 128.0],
            hide_server_in_user_agent: false,
            instance_blocklist: Vec::new(),
            state_dir: default_state_dir(),
            lock_file: None,
            lock_hours: 24,
            on_start: None,
            on_done: None,
            on_fail: None,
            log_level: "DEBUG".to_string(),
            log_format: "%(asctime)s: %(message)s".to_string(),
            log_page_loads: false,
        }
    }
}

impl Config {
    /// In case someone provided the server name as a url instead, strip the
    /// scheme and any trailing slash.
    pub fn normalize_server(server: &str) -> String {
        let pattern = Regex::new(r"^(https://)?([^/]*)/?$").unwrap();
        match pattern.captures(server) {
            Some(captures) => captures
                .get(2)
                .map(|m| m.as_str())
                .unwrap_or(server)
                .to_string(),
            None => server.to_string(),
        }
    }

    pub fn lock_path(&self) -> PathBuf {
        self.lock_file
            .clone()
            .unwrap_or_else(|| self.state_dir.join("lock.lock"))
    }

    /// Which tasks a run will do, in the order it will do them. An empty
    /// list means the run has nothing to do and will finish immediately.
    pub fn enabled_tasks(&self) -> Vec<&'static str> {
        let mut tasks = Vec::new();
        if self.from_lists {
            tasks.push("lists");
        }
        if self.reply_interval_in_hours > 0 {
            tasks.push("reply context");
        }
        if self.home_timeline_length > 0 {
            tasks.push("home timeline");
        }
        if self.max_followings > 0 {
            tasks.push("followings");
        }
        if self.max_followers > 0 {
            tasks.push("followers");
        }
        if self.max_follow_requests > 0 {
            tasks.push("follow requests");
        }
        if self.from_notifications > 0 {
            tasks.push("notifications");
        }
        if self.max_bookmarks > 0 {
            tasks.push("bookmarks");
        }
        if self.max_favorites > 0 {
            tasks.push("favorites");
        }
        tasks
    }

    /// Overlay raw values onto this config, coercing them the same way
    /// `Config::from_values` does, and normalizing the server name.
    pub fn apply_values(&mut self, values: &RawValues) -> Result<()> {
        apply(self, values)?;
        self.server = Config::normalize_server(&self.server);
        Ok(())
    }

    /// The whole effective configuration as raw values, ready to be stored
    /// in the database. Options that are unset (`None`) are omitted, so
    /// a cleared option stays cleared when the config is read back.
    pub fn to_raw_values(&self) -> RawValues {
        let mut values = RawValues::new();
        let mut put = |key: &str, value: Value| {
            values.insert(key.to_string(), value);
        };
        put("server", Value::String(self.server.clone()));
        put(
            "access_tokens",
            Value::Array(
                self.access_tokens
                    .iter()
                    .map(|token| Value::String(token.clone()))
                    .collect(),
            ),
        );
        put(
            "reply_interval_in_hours",
            Value::from(self.reply_interval_in_hours),
        );
        put(
            "home_timeline_length",
            Value::from(self.home_timeline_length),
        );
        put("streaming", Value::Bool(self.streaming));
        put("user", Value::String(self.user.clone()));
        put("max_followings", Value::from(self.max_followings));
        put("max_followers", Value::from(self.max_followers));
        put("max_follow_requests", Value::from(self.max_follow_requests));
        put("max_bookmarks", Value::from(self.max_bookmarks));
        put("max_favorites", Value::from(self.max_favorites));
        put("from_notifications", Value::from(self.from_notifications));
        put("from_lists", Value::Bool(self.from_lists));
        put("max_list_length", Value::from(self.max_list_length));
        put("max_list_accounts", Value::from(self.max_list_accounts));
        put(
            "max_posts_per_account",
            Value::from(self.max_posts_per_account),
        );
        put(
            "backfill_with_context",
            Value::Bool(self.backfill_with_context),
        );
        put(
            "backfill_mentioned_users",
            Value::Bool(self.backfill_mentioned_users),
        );
        put("respect_robots_txt", Value::Bool(self.respect_robots_txt));
        put(
            "respect_user_opt_out",
            Value::Bool(self.respect_user_opt_out),
        );
        put(
            "remember_users_for_hours",
            Value::from(self.remember_users_for_hours),
        );
        put(
            "remember_hosts_for_days",
            Value::from(self.remember_hosts_for_days),
        );
        put("http_timeout", Value::from(self.http_timeout));
        put(
            "rate_limit_cooldown",
            Value::Array(
                self.rate_limit_cooldown
                    .iter()
                    .map(|seconds| Value::from(*seconds))
                    .collect(),
            ),
        );
        put(
            "hide_server_in_user_agent",
            Value::Bool(self.hide_server_in_user_agent),
        );
        put(
            "instance_blocklist",
            Value::Array(
                self.instance_blocklist
                    .iter()
                    .map(|host| Value::String(host.clone()))
                    .collect(),
            ),
        );
        put(
            "state_dir",
            Value::String(self.state_dir.display().to_string()),
        );
        if let Some(lock_file) = &self.lock_file {
            put("lock_file", Value::String(lock_file.display().to_string()));
        }
        put("lock_hours", Value::from(self.lock_hours));
        if let Some(on_start) = &self.on_start {
            put("on_start", Value::String(on_start.clone()));
        }
        if let Some(on_done) = &self.on_done {
            put("on_done", Value::String(on_done.clone()));
        }
        if let Some(on_fail) = &self.on_fail {
            put("on_fail", Value::String(on_fail.clone()));
        }
        put("log_level", Value::String(self.log_level.clone()));
        put("log_format", Value::String(self.log_format.clone()));
        put("log_page_loads", Value::Bool(self.log_page_loads));
        values
    }
}

/// The raw, uncoerced configuration values stored in the database, keyed by
/// option name.
pub type RawValues = BTreeMap<String, Value>;

impl Config {
    /// Build a Config from the values stored in the database.
    pub fn from_values(stored: RawValues) -> Result<Config> {
        Self::from_values_with(stored, true)
    }

    /// Like [`Config::from_values`], but a missing server or access token is
    /// not an error: the web UI starts with an empty configuration, and the
    /// values are filled in through its config page.
    pub fn from_values_lenient(stored: RawValues) -> Result<Config> {
        Self::from_values_with(stored, false)
    }

    fn from_values_with(stored: RawValues, require_server_and_token: bool) -> Result<Config> {
        if require_server_and_token
            && (!stored.contains_key("server")
                || stored
                    .get("access_tokens")
                    .map(is_empty_list)
                    .unwrap_or(true))
        {
            return Err(Error::Config(
                "You must supply at least a server name and an access token".to_string(),
            ));
        }

        let mut config = Config::default();
        config.apply_values(&stored)?;
        Ok(config)
    }
}

fn is_empty_list(value: &Value) -> bool {
    match value {
        Value::Array(items) => items.is_empty(),
        Value::Null => true,
        _ => false,
    }
}

pub fn is_known_option(name: &str) -> bool {
    matches!(
        name,
        "server"
            | "access_tokens"
            | "reply_interval_in_hours"
            | "home_timeline_length"
            | "streaming"
            | "user"
            | "max_followings"
            | "max_followers"
            | "max_follow_requests"
            | "max_bookmarks"
            | "max_favorites"
            | "from_notifications"
            | "from_lists"
            | "max_list_length"
            | "max_list_accounts"
            | "max_posts_per_account"
            | "backfill_with_context"
            | "backfill_mentioned_users"
            | "respect_robots_txt"
            | "respect_user_opt_out"
            | "remember_users_for_hours"
            | "remember_hosts_for_days"
            | "http_timeout"
            | "rate_limit_cooldown"
            | "hide_server_in_user_agent"
            | "instance_blocklist"
            | "state_dir"
            | "lock_file"
            | "lock_hours"
            | "on_start"
            | "on_done"
            | "on_fail"
            | "log_level"
            | "log_format"
            | "log_page_loads"
    )
}

/// What an option does, as the original documentation puts it — shown as the
/// config page's hover tooltips.
pub fn option_description(name: &str) -> Option<&'static str> {
    Some(match name {
        "server" => "Required: the name of your server (e.g. mstdn.thms.uk)",
        "access_tokens" => {
            "Required: tokens generated at https://<server>/settings/applications, needing read:search, read:statuses and admin:read:accounts scopes. Add one per user whose timelines you fetch."
        }
        "reply_interval_in_hours" => {
            "Fetch remote replies to posts that have received replies from users on your own instance in this period"
        }
        "home_timeline_length" => {
            "Look for replies to posts in the API key owner's home timeline, up to this many posts"
        }
        "streaming" => {
            "Stream the home timeline over a long-lived connection, and fetch context for new posts as they arrive instead of waiting for the next run. Serve mode only, with the first access token; applies on restart (default: disabled)"
        }
        "user" => {
            "Use together with max_followings or max_followers to backfill a specific user's followings/followers. If omitted, we use the account that owns the access token."
        }
        "max_followings" => {
            "Backfill posts for new accounts followed by user. At most this many followings' posts"
        }
        "max_followers" => {
            "Backfill posts for new accounts following user. At most this many followers' posts"
        }
        "max_follow_requests" => {
            "Backfill posts of the API key owners' pending follow requests. At most this many requesters' posts"
        }
        "max_bookmarks" => {
            "Fetch remote replies to the API key owners' bookmarks. At most this many bookmarks"
        }
        "max_favorites" => {
            "Fetch remote replies to the API key owners' favorites. At most this many favorites"
        }
        "from_notifications" => {
            "Backfill accounts of anyone appearing in your notifications, during the last hours"
        }
        "from_lists" => {
            "Fetch missing replies and/or backfill accounts from your lists. Disabled by default"
        }
        "max_list_length" => {
            "How many posts to fetch replies for in each list. Ignored unless from_lists is set. Set to 0 to only backfill profiles in lists"
        }
        "max_list_accounts" => {
            "How many accounts to backfill for in each list. Ignored unless from_lists is set. Set to 0 to only fetch replies in lists"
        }
        "max_posts_per_account" => {
            "How many posts to fetch from each account we backfill. Above the 40 fetched by default means more requests to the remote server, and to your own server to pull each post in, so raising this makes FediFetcher considerably slower"
        }
        "backfill_with_context" => {
            "Fetch remote replies when backfilling profiles (default: enabled)"
        }
        "backfill_mentioned_users" => {
            "Backfill any mentioned users when fetching remote replies to timeline posts (default: enabled)"
        }
        "respect_robots_txt" => {
            "Respect robots.txt when fetching from remote servers (default: enabled)"
        }
        "respect_user_opt_out" => {
            "Skip accounts that have opted out (nobot tag, or not indexable or discoverable) when backfilling (default: enabled)"
        }
        "remember_users_for_hours" => {
            "How long to remember users that you aren't following for, before trying to backfill them again"
        }
        "remember_hosts_for_days" => "How long to remember host info for, before checking again",
        "http_timeout" => "The timeout for any HTTP requests to your own, or other instances",
        "rate_limit_cooldown" => {
            "Seconds to wait between retries when a server rate limits us (HTTP 429): a comma-separated list, one entry per retry step, tried in order. When the list runs out, the request fails. Default: 0.5,2,8,32,128"
        }
        "hide_server_in_user_agent" => {
            "Omit your server's name from the User-Agent FediFetcher sends to other servers (default: disabled)"
        }
        "instance_blocklist" => {
            "A comma-separated list of instances that FediFetcher should never try to connect to"
        }
        "state_dir" => {
            "Directory to store persistent files and possibly the lock file. Defaults to ./artifacts if that directory exists, and to $XDG_STATE_HOME/fedifetcher (usually ~/.local/state/fedifetcher) otherwise"
        }
        "lock_file" => "Location of the lock file. Defaults to lock.lock in state_dir",
        "lock_hours" => {
            "How long a lock held by a live process is respected. Locks held by a dead process, or left by an older version, are reclaimed immediately"
        }
        "on_start" => {
            "Pinged when processing is starting — use for 'dead man switch' monitoring of your task"
        }
        "on_done" => {
            "Pinged when processing has completed — use for 'dead man switch' monitoring of your task"
        }
        "on_fail" => {
            "Pinged when processing has failed — use for 'dead man switch' monitoring of your task"
        }
        "log_level" => {
            "Severity of events to log: DEBUG, INFO, WARNING, ERROR or CRITICAL. Applies on restart"
        }
        "log_format" => "The log format. Applies on restart",
        "log_page_loads" => {
            "Log a line for every page load of the web UI, at DEBUG (default: disabled)"
        }
        _ => return None,
    })
}

fn apply(config: &mut Config, values: &RawValues) -> Result<()> {
    for (name, value) in values {
        let result = match name.as_str() {
            "server" => coerce_string(value).map(|v| config.server = v),
            "access_tokens" => coerce_list(value).map(|v| config.access_tokens = v),
            "reply_interval_in_hours" => {
                coerce_int(value).map(|v| config.reply_interval_in_hours = v)
            }
            "home_timeline_length" => coerce_int(value).map(|v| config.home_timeline_length = v),
            "streaming" => coerce_bool(value).map(|v| config.streaming = v),
            "user" => coerce_string(value).map(|v| config.user = v),
            "max_followings" => coerce_int(value).map(|v| config.max_followings = v),
            "max_followers" => coerce_int(value).map(|v| config.max_followers = v),
            "max_follow_requests" => coerce_int(value).map(|v| config.max_follow_requests = v),
            "max_bookmarks" => coerce_int(value).map(|v| config.max_bookmarks = v),
            "max_favorites" => coerce_int(value).map(|v| config.max_favorites = v),
            "from_notifications" => coerce_int(value).map(|v| config.from_notifications = v),
            "from_lists" => coerce_bool(value).map(|v| config.from_lists = v),
            "max_list_length" => coerce_int(value).map(|v| config.max_list_length = v),
            "max_list_accounts" => coerce_int(value).map(|v| config.max_list_accounts = v),
            "max_posts_per_account" => coerce_int(value).map(|v| config.max_posts_per_account = v),
            "backfill_with_context" => coerce_bool(value).map(|v| config.backfill_with_context = v),
            "backfill_mentioned_users" => {
                coerce_bool(value).map(|v| config.backfill_mentioned_users = v)
            }
            "respect_robots_txt" => coerce_bool(value).map(|v| config.respect_robots_txt = v),
            "respect_user_opt_out" => coerce_bool(value).map(|v| config.respect_user_opt_out = v),
            "remember_users_for_hours" => {
                coerce_int(value).map(|v| config.remember_users_for_hours = v)
            }
            "remember_hosts_for_days" => {
                coerce_int(value).map(|v| config.remember_hosts_for_days = v)
            }
            "http_timeout" => coerce_int(value).map(|v| config.http_timeout = v),
            "rate_limit_cooldown" => coerce_seconds(value).map(|v| config.rate_limit_cooldown = v),
            "hide_server_in_user_agent" => {
                coerce_bool(value).map(|v| config.hide_server_in_user_agent = v)
            }
            "instance_blocklist" => coerce_list(value).map(|v| config.instance_blocklist = v),
            "state_dir" => coerce_path(value).map(|v| config.state_dir = v),
            "lock_file" => coerce_optional_path(value).map(|v| config.lock_file = v),
            "lock_hours" => coerce_int(value).map(|v| config.lock_hours = v),
            "on_start" => coerce_optional_string(value).map(|v| config.on_start = v),
            "on_done" => coerce_optional_string(value).map(|v| config.on_done = v),
            "on_fail" => coerce_optional_string(value).map(|v| config.on_fail = v),
            "log_level" => coerce_string(value).map(|v| config.log_level = v),
            "log_format" => coerce_string(value).map(|v| config.log_format = v),
            "log_page_loads" => coerce_bool(value).map(|v| config.log_page_loads = v),
            // unknown options are ignored, as in the Python original
            _ => Ok(()),
        };
        result.map_err(|ex| Error::Config(format!("Cannot read option {name}: {ex}")))?;
    }
    Ok(())
}

fn coerce_bool(value: &Value) -> Result<bool> {
    match value {
        Value::Bool(b) => Ok(*b),
        Value::Number(n) => Ok(n.as_i64().unwrap_or(0) != 0),
        Value::String(text) => {
            let text = text.trim().to_lowercase();
            if TRUTHY.contains(&text.as_str()) {
                Ok(true)
            } else if FALSEY.contains(&text.as_str()) {
                Ok(false)
            } else {
                Err(Error::Config(format!("{text:?} is not a yes/no value")))
            }
        }
        _ => Err(Error::Config(format!("{value} is not a yes/no value"))),
    }
}

fn coerce_int(value: &Value) -> Result<i64> {
    match value {
        Value::Number(n) => n
            .as_i64()
            .ok_or_else(|| Error::Config(format!("{value} is not an integer"))),
        Value::Bool(b) => Ok(i64::from(*b)),
        Value::String(text) => text
            .trim()
            .parse::<i64>()
            .map_err(|_| Error::Config(format!("{text:?} is not an integer"))),
        _ => Err(Error::Config(format!("{value} is not an integer"))),
    }
}

fn coerce_string(value: &Value) -> Result<String> {
    match value {
        Value::String(s) => Ok(s.clone()),
        Value::Number(n) => Ok(n.to_string()),
        Value::Bool(b) => Ok(b.to_string()),
        _ => Err(Error::Config(format!("{value} is not a string"))),
    }
}

fn coerce_optional_string(value: &Value) -> Result<Option<String>> {
    match value {
        Value::Null => Ok(None),
        other => coerce_string(other).map(Some),
    }
}

fn coerce_path(value: &Value) -> Result<PathBuf> {
    match value {
        Value::String(s) => Ok(PathBuf::from(s)),
        _ => Err(Error::Config(format!("{value} is not a path"))),
    }
}

fn coerce_optional_path(value: &Value) -> Result<Option<PathBuf>> {
    match value {
        Value::Null => Ok(None),
        other => coerce_path(other).map(Some),
    }
}

fn coerce_list(value: &Value) -> Result<Vec<String>> {
    match value {
        // a comma-separated string, as on the command line
        Value::String(text) => Ok(text
            .split(',')
            .map(str::trim)
            .filter(|part| !part.is_empty())
            .map(str::to_string)
            .collect()),
        Value::Array(items) => items
            .iter()
            .map(|item| match item {
                Value::String(s) => Ok(s.clone()),
                Value::Number(n) => Ok(n.to_string()),
                _ => Err(Error::Config(format!("{item} is not a string"))),
            })
            .collect(),
        _ => Err(Error::Config(format!("{value} is not a list of values"))),
    }
}

/// A list of seconds, as a comma-separated string or an array of numbers.
/// Every entry must be a finite, non-negative number: a negative or NaN
/// delay would panic when the wait is built.
fn coerce_seconds(value: &Value) -> Result<Vec<f64>> {
    let parse = |text: &str| {
        let seconds: f64 = text
            .trim()
            .parse()
            .map_err(|_| Error::Config(format!("{text:?} is not a number of seconds")))?;
        if seconds.is_finite() && seconds >= 0.0 {
            Ok(seconds)
        } else {
            Err(Error::Config(format!(
                "{text:?} is not a number of seconds"
            )))
        }
    };
    match value {
        // a comma-separated string, as on the command line
        Value::String(text) => text
            .split(',')
            .map(str::trim)
            .filter(|part| !part.is_empty())
            .map(parse)
            .collect(),
        Value::Array(items) => items
            .iter()
            .map(|item| match item {
                Value::Number(n) => n
                    .as_f64()
                    .ok_or_else(|| Error::Config(format!("{item} is not a number of seconds")))
                    .and_then(|seconds| {
                        if seconds.is_finite() && seconds >= 0.0 {
                            Ok(seconds)
                        } else {
                            Err(Error::Config(format!("{item} is not a number of seconds")))
                        }
                    }),
                Value::String(text) => parse(text),
                _ => Err(Error::Config(format!("{item} is not a number of seconds"))),
            })
            .collect(),
        _ => Err(Error::Config(format!("{value} is not a list of seconds"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn values(pairs: Vec<(&str, Value)>) -> RawValues {
        pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect()
    }

    fn minimal() -> RawValues {
        values(vec![
            ("server", json!("example.com")),
            ("access_tokens", json!(["token1"])),
        ])
    }

    #[test]
    fn requires_server_and_token() {
        assert!(Config::from_values(RawValues::new()).is_err());
        assert!(Config::from_values(values(vec![("server", json!("x"))])).is_err());
        assert!(Config::from_values(minimal()).is_ok());
    }

    #[test]
    fn every_known_option_has_a_description() {
        for name in [
            "server",
            "access_tokens",
            "reply_interval_in_hours",
            "home_timeline_length",
            "streaming",
            "user",
            "max_followings",
            "max_followers",
            "max_follow_requests",
            "max_bookmarks",
            "max_favorites",
            "from_notifications",
            "from_lists",
            "max_list_length",
            "max_list_accounts",
            "max_posts_per_account",
            "backfill_with_context",
            "backfill_mentioned_users",
            "respect_robots_txt",
            "respect_user_opt_out",
            "remember_users_for_hours",
            "remember_hosts_for_days",
            "http_timeout",
            "rate_limit_cooldown",
            "hide_server_in_user_agent",
            "instance_blocklist",
            "state_dir",
            "lock_file",
            "lock_hours",
            "on_start",
            "on_done",
            "on_fail",
            "log_level",
            "log_format",
            "log_page_loads",
        ] {
            assert!(is_known_option(name), "{name}");
            assert!(option_description(name).is_some(), "{name} has no tooltip");
        }
        assert_eq!(option_description("not_an_option"), None);
    }

    #[test]
    fn the_user_agent_option_round_trips() {
        let config = Config::from_values(values(vec![
            ("server", json!("example.social")),
            ("access_tokens", json!(["t"])),
            ("hide_server_in_user_agent", json!(true)),
        ]))
        .unwrap();
        assert!(config.hide_server_in_user_agent);
        assert!(
            !Config::default().hide_server_in_user_agent,
            "the server is named by default"
        );
    }

    #[test]
    fn server_is_normalized() {
        let mut raw = minimal();
        raw.insert("server".to_string(), json!("https://mstdn.thms.uk/"));
        let config = Config::from_values(raw).unwrap();
        assert_eq!(config.server, "mstdn.thms.uk");
    }

    #[test]
    fn bool_coercion_accepts_yes_no_values() {
        for truthy in ["1", "true", "yes", "on", " TRUE "] {
            let config = Config::from_values(values(vec![
                ("server", json!("x")),
                ("access_tokens", json!(["t"])),
                ("from_lists", json!(truthy)),
            ]))
            .unwrap();
            assert!(config.from_lists, "{truthy}");
        }
        for falsy in ["0", "false", "no", "off", ""] {
            let config = Config::from_values(values(vec![
                ("server", json!("x")),
                ("access_tokens", json!(["t"])),
                ("from_lists", json!(falsy)),
            ]))
            .unwrap();
            assert!(!config.from_lists, "{falsy}");
        }
        assert!(
            Config::from_values(values(vec![
                ("server", json!("x")),
                ("access_tokens", json!(["t"])),
                ("from_lists", json!("maybe")),
            ]))
            .is_err()
        );
    }

    #[test]
    fn defaults_match_the_documented_values() {
        let config = Config::from_values(minimal()).unwrap();
        assert_eq!(config.max_posts_per_account, 40);
        assert!(config.backfill_with_context);
        assert!(config.backfill_mentioned_users);
        assert!(config.respect_robots_txt);
        assert!(config.respect_user_opt_out);
        assert_eq!(config.remember_users_for_hours, 24 * 7);
        assert_eq!(config.remember_hosts_for_days, 30);
        assert_eq!(config.http_timeout, 5);
        assert_eq!(config.max_list_length, 100);
        assert_eq!(config.max_list_accounts, 10);
        assert_eq!(config.lock_hours, 24);
        assert!(!config.from_lists);
        assert!(!config.streaming, "streaming is off by default");
        assert_eq!(config.log_level, "DEBUG");
        assert_eq!(config.log_format, "%(asctime)s: %(message)s");
        assert!(
            !config.log_page_loads,
            "page loads are not logged by default"
        );
        assert_eq!(config.lock_path(), config.state_dir.join("lock.lock"));
    }

    #[test]
    fn list_options_accept_comma_strings_and_arrays() {
        let config = Config::from_values(values(vec![
            ("server", json!("x")),
            ("access_tokens", json!(["t"])),
            ("instance_blocklist", json!("a.example, b.example")),
        ]))
        .unwrap();
        assert_eq!(config.instance_blocklist, vec!["a.example", "b.example"]);

        let config = Config::from_values(values(vec![
            ("server", json!("x")),
            ("access_tokens", json!(["t"])),
            ("instance_blocklist", json!(["a.example", "b.example"])),
        ]))
        .unwrap();
        assert_eq!(config.instance_blocklist, vec!["a.example", "b.example"]);
    }

    #[test]
    fn the_rate_limit_cooldown_defaults_to_the_historical_series() {
        let config = Config::from_values(minimal()).unwrap();
        // what the hardcoded 0.5s backoff growing fourfold each step waited
        assert_eq!(config.rate_limit_cooldown, vec![0.5, 2.0, 8.0, 32.0, 128.0]);
    }

    #[test]
    fn the_rate_limit_cooldown_accepts_comma_strings_and_arrays() {
        let config = Config::from_values(values(vec![
            ("server", json!("x")),
            ("access_tokens", json!(["t"])),
            ("rate_limit_cooldown", json!("1, 2.5, 10")),
        ]))
        .unwrap();
        assert_eq!(config.rate_limit_cooldown, vec![1.0, 2.5, 10.0]);

        let config = Config::from_values(values(vec![
            ("server", json!("x")),
            ("access_tokens", json!(["t"])),
            ("rate_limit_cooldown", json!([1, 2.5, 10])),
        ]))
        .unwrap();
        assert_eq!(config.rate_limit_cooldown, vec![1.0, 2.5, 10.0]);

        // an empty list means no retries at all
        let config = Config::from_values(values(vec![
            ("server", json!("x")),
            ("access_tokens", json!(["t"])),
            ("rate_limit_cooldown", json!("")),
        ]))
        .unwrap();
        assert!(config.rate_limit_cooldown.is_empty());
    }

    #[test]
    fn the_rate_limit_cooldown_rejects_impossible_delays() {
        for bad in [json!("soon"), json!([-1, 2]), json!(["nan"]), json!(42)] {
            let result = Config::from_values(values(vec![
                ("server", json!("x")),
                ("access_tokens", json!(["t"])),
                ("rate_limit_cooldown", bad.clone()),
            ]));
            assert!(result.is_err(), "{bad} should not parse");
        }
    }

    #[test]
    fn the_rate_limit_cooldown_round_trips_through_raw_values() {
        let mut config = Config::from_values(minimal()).unwrap();
        config.rate_limit_cooldown = vec![0.25, 1.0, 60.0];
        let raw = config.to_raw_values();
        let rebuilt = Config::from_values(raw).unwrap();
        assert_eq!(rebuilt.rate_limit_cooldown, vec![0.25, 1.0, 60.0]);
    }

    #[test]
    fn unknown_options_are_ignored() {
        let config = Config::from_values(values(vec![
            ("server", json!("x")),
            ("access_tokens", json!(["t"])),
            ("nonsense", json!(1)),
        ]))
        .unwrap();
        assert_eq!(config.server, "x");
    }

    #[test]
    fn enabled_tasks_follow_the_config() {
        let mut config = Config::from_values(minimal()).unwrap();
        assert!(
            config.enabled_tasks().is_empty(),
            "the defaults enable nothing"
        );
        config.from_lists = true;
        config.reply_interval_in_hours = 48;
        config.max_followers = 5;
        assert_eq!(
            config.enabled_tasks(),
            vec!["lists", "reply context", "followers"]
        );
    }

    #[test]
    fn lenient_build_allows_an_empty_config() {
        let config = Config::from_values_lenient(RawValues::new()).unwrap();
        assert_eq!(config.server, "");
        assert!(config.access_tokens.is_empty());

        let config = Config::from_values_lenient(minimal()).unwrap();
        assert_eq!(config.server, "example.com");
        assert_eq!(config.access_tokens, vec!["token1".to_string()]);
    }
}
