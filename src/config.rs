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
    /// Send this User-Agent to other servers instead of the default one,
    /// as-is; when set, `hide_server_in_user_agent` is ignored
    /// (default: unset)
    pub user_agent_override: Option<String>,
    /// Show the debug tooling: the Debug page, the Data page's Actions
    /// column, and the options that stand between FediFetcher and the
    /// politeness rules. Turning it off writes those options' defaults
    /// back (default: enabled)
    pub debug_mode: bool,
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
            user_agent_override: None,
            debug_mode: true,
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

    /// Write the debug-gated options' defaults back: what the config page
    /// does when debug mode is turned off, so the options it hides are
    /// always the documented defaults.
    pub fn reset_debug_gated(&mut self) {
        let defaults = Config::default();
        self.respect_robots_txt = defaults.respect_robots_txt;
        self.respect_user_opt_out = defaults.respect_user_opt_out;
        self.hide_server_in_user_agent = defaults.hide_server_in_user_agent;
        self.user_agent_override = defaults.user_agent_override.clone();
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
        if let Some(agent) = &self.user_agent_override {
            put("user_agent_override", Value::String(agent.clone()));
        }
        put("debug_mode", Value::Bool(self.debug_mode));
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
            | "user_agent_override"
            | "debug_mode"
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
/// config page's hover tooltips, followed by the option's documented default
/// and the API scopes it spends. The lines are newlines in the title
/// attribute, which browsers render as line breaks; there is no bolding in
/// a title attribute.
pub fn option_description(name: &str) -> Option<String> {
    let base: &'static str = base_description(name)?;
    let mut text = String::from(base);
    if name != "server"
        && name != "access_tokens"
        && let Some(default) = option_default(name)
    {
        text.push_str("\nDefault: ");
        text.push_str(&default_spelling(&default));
    }
    if let Some(scopes) = option_scopes(name) {
        text.push_str("\nScopes: ");
        text.push_str(scopes);
    }
    Some(text)
}

/// The default, spelled the way the README's table spells it.
fn default_spelling(value: &Value) -> String {
    match value {
        Value::Bool(enabled) => {
            format!(
                "{enabled} ({})",
                if *enabled { "enabled" } else { "disabled" }
            )
        }
        Value::Number(n) if n.as_i64() == Some(0) => "0 (off)".to_string(),
        Value::Array(items) => items
            .iter()
            .map(|item| item.to_string())
            .collect::<Vec<_>>()
            .join(","),
        other => other.to_string(),
    }
}

/// The API scope an option's task spends, with how the ask treats it.
fn option_scopes(name: &str) -> Option<&'static str> {
    Some(match name {
        "reply_interval_in_hours" => "admin:read:accounts (asked only while this is enabled)",
        "from_lists" | "max_list_length" | "max_list_accounts" => {
            "read:lists (asked only while lists are enabled)"
        }
        "from_notifications" => "read:notifications (asked only while this is enabled)",
        "max_bookmarks" => "read:bookmarks (asked only while this is enabled)",
        "max_favorites" => "read:favourites (asked only while this is enabled)",
        "max_followings" | "max_followers" | "max_follow_requests" | "user" => {
            "read:follows (asked only while a follow backfill is enabled)"
        }
        "home_timeline_length" | "streaming" => "read:statuses (always granted)",
        _ => return None,
    })
}

fn base_description(name: &str) -> Option<&'static str> {
    Some(match name {
        "server" => "Required: the name of your server (e.g. mstdn.thms.uk)",
        "access_tokens" => {
            "Required: one token per user whose timelines you fetch — connected with the button above, or created at https://<server>/settings/applications with the scopes the enabled tasks need."
        }
        "reply_interval_in_hours" => {
            "Fetch remote replies to posts that have received replies from users on your own instance in this period"
        }
        "home_timeline_length" => {
            "Look for replies to posts in the API key owner's home timeline, up to this many posts"
        }
        "streaming" => {
            "Stream the home timeline over a long-lived connection, and fetch context for new posts as they arrive instead of waiting for the next run. Serve mode only, with the first access token; applies on restart"
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
        "from_lists" => "Fetch missing replies and/or backfill accounts from your lists",
        "max_list_length" => {
            "How many posts to fetch replies for in each list. Ignored unless from_lists is set. Set to 0 to only backfill profiles in lists"
        }
        "max_list_accounts" => {
            "How many accounts to backfill for in each list. Ignored unless from_lists is set. Set to 0 to only fetch replies in lists"
        }
        "max_posts_per_account" => {
            "How many posts to fetch from each account we backfill. Above the 40 fetched by default means more requests to the remote server, and to your own server to pull each post in, so raising this makes FediFetcher considerably slower"
        }
        "backfill_with_context" => "Fetch remote replies when backfilling profiles",
        "backfill_mentioned_users" => {
            "Backfill any mentioned users when fetching remote replies to timeline posts"
        }
        "respect_robots_txt" => "Respect robots.txt when fetching from remote servers",
        "respect_user_opt_out" => {
            "Skip accounts that have opted out (nobot tag, or not indexable or discoverable) when backfilling"
        }
        "remember_users_for_hours" => {
            "How long to remember users that you aren't following for, before trying to backfill them again"
        }
        "remember_hosts_for_days" => "How long to remember host info for, before checking again",
        "http_timeout" => "The timeout for any HTTP requests to your own, or other instances",
        "rate_limit_cooldown" => {
            "Seconds to wait between retries when a server rate limits us (HTTP 429): a comma-separated list, one entry per retry step, tried in order. When the list runs out, the request fails"
        }
        "hide_server_in_user_agent" => {
            "Omit your server's name from the User-Agent FediFetcher sends to other servers"
        }
        "user_agent_override" => {
            "Send this User-Agent to other servers instead of the default one, as-is. When set, hide_server_in_user_agent is ignored. Leave empty to use the default"
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
        "log_page_loads" => "Log a line for every page load of the web UI, at DEBUG",
        "debug_mode" => {
            "Show the debug tooling: the Debug page, the Data page's Actions column, and the options that stand between FediFetcher and the politeness rules (respect_robots_txt, respect_user_opt_out, hide_server_in_user_agent, user_agent_override). Turning it off writes those options' defaults back, so the hidden options are the documented defaults"
        }
        _ => return None,
    })
}

/// The options the debug mode gates: the ones that stand between
/// FediFetcher and the politeness rules, plus the User-Agent override.
/// Hidden on the config page while debug mode is off, and written back to
/// their defaults when debug mode is turned off.
pub fn debug_gated_options() -> &'static [&'static str] {
    &[
        "respect_robots_txt",
        "respect_user_opt_out",
        "hide_server_in_user_agent",
        "user_agent_override",
    ]
}

/// Options whose empty form field means "back to the documented default":
/// the numbers, booleans, and lists. The optional strings' empty means
/// unset instead, and server and access_tokens are required outright.
pub fn empty_resets_to_default(name: &str) -> bool {
    matches!(
        name,
        "reply_interval_in_hours"
            | "home_timeline_length"
            | "streaming"
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
            | "debug_mode"
            | "instance_blocklist"
            | "log_page_loads"
    )
}

/// An option's default value, as the crate's own defaults spell it — the
/// one table of defaults is `Config::default`, which the README documents
/// and a test keeps honest.
pub fn option_default(name: &str) -> Option<Value> {
    Config::default().to_raw_values().get(name).cloned()
}

fn apply(config: &mut Config, values: &RawValues) -> Result<()> {
    for (name, value) in values {
        // a cleared field for the numbers, booleans, and lists means the
        // documented default — the same value the field starts with — not
        // a parse error; the optional strings keep their "empty is unset"
        let substituted;
        let value = if value == &Value::String(String::new()) && empty_resets_to_default(name) {
            substituted = option_default(name).unwrap_or(Value::Null);
            &substituted
        } else {
            value
        };
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
            "user_agent_override" => {
                coerce_optional_string(value).map(|v| config.user_agent_override = v)
            }
            "debug_mode" => coerce_bool(value).map(|v| config.debug_mode = v),
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
mod tests;
