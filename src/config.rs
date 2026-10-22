//! Configuration: one struct, read from defaults, a config file, the
//! environment and the command line, in increasing order of precedence.
//!
//! The shapes mirror the Python original: a JSON (or TOML) config file, `ff_*`
//! environment variables, and command line flags, with every option spelled the
//! same way in all three places.

use std::collections::BTreeMap;
use std::path::PathBuf;

use regex::Regex;
use serde_json::Value;

use crate::error::ConfigError;

const TRUTHY: &[&str] = &["1", "true", "yes", "on"];
const FALSEY: &[&str] = &["0", "false", "no", "off", ""];

const ENV_PREFIX: &str = "ff_";
const ACCESS_TOKEN_ENV_PREFIX: &str = "ff_access_token";

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
    if let Ok(xdg) = std::env::var("XDG_STATE_HOME") {
        if !xdg.is_empty() {
            return PathBuf::from(xdg).join("fedifetcher");
        }
    }
    if let Some(home) = std::env::var_os("HOME") {
        return PathBuf::from(home).join(".local").join("state").join("fedifetcher");
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
    /// Fetch remote replies to the API key owner's favourites, at most this many
    pub max_favourites: i64,
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
}

impl Default for Config {
    fn default() -> Self {
        Config {
            server: String::new(),
            access_tokens: Vec::new(),
            reply_interval_in_hours: 0,
            home_timeline_length: 0,
            user: String::new(),
            max_followings: 0,
            max_followers: 0,
            max_follow_requests: 0,
            max_bookmarks: 0,
            max_favourites: 0,
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
            instance_blocklist: Vec::new(),
            state_dir: default_state_dir(),
            lock_file: None,
            lock_hours: 24,
            on_start: None,
            on_done: None,
            on_fail: None,
            log_level: "DEBUG".to_string(),
            log_format: "%(asctime)s: %(message)s".to_string(),
        }
    }
}

impl Config {
    /// In case someone provided the server name as a url instead, strip the
    /// scheme and any trailing slash.
    pub fn normalize_server(server: &str) -> String {
        let pattern = Regex::new(r"^(https://)?([^/]*)/?$").unwrap();
        match pattern.captures(server) {
            Some(captures) => captures.get(2).map(|m| m.as_str()).unwrap_or(server).to_string(),
            None => server.to_string(),
        }
    }

    pub fn lock_path(&self) -> PathBuf {
        self.lock_file.clone().unwrap_or_else(|| self.state_dir.join("lock.lock"))
    }

    pub fn seen_urls_file(&self) -> PathBuf {
        self.state_dir.join("seen_urls")
    }

    pub fn replied_toot_server_ids_file(&self) -> PathBuf {
        self.state_dir.join("replied_toot_server_ids")
    }

    pub fn known_followings_file(&self) -> PathBuf {
        self.state_dir.join("known_followings")
    }

    pub fn recently_checked_users_file(&self) -> PathBuf {
        self.state_dir.join("recently_checked_users")
    }

    pub fn seen_hosts_file(&self) -> PathBuf {
        self.state_dir.join("seen_hosts")
    }

    pub fn recently_checked_contexts_file(&self) -> PathBuf {
        self.state_dir.join("recent_context")
    }
}

/// The raw, uncoerced values from one source, keyed by option name.
pub type RawValues = BTreeMap<String, Value>;

/// Build a Config from the command line, environment and config file.
///
/// Later sources win: `file`, then `environ`, then `cli`.
pub fn build_config(file: RawValues, environ: RawValues, cli: RawValues) -> Result<Config, ConfigError> {
    let mut values: RawValues = RawValues::new();
    for source in [file, environ, cli] {
        for (key, value) in source {
            values.insert(key, value);
        }
    }

    if !values.contains_key("server") || values.get("access_tokens").map(is_empty_list).unwrap_or(true) {
        return Err(ConfigError(
            "You must supply at least a server name and an access token".to_string(),
        ));
    }

    let mut config = Config::default();
    apply(&mut config, &values)?;
    config.server = Config::normalize_server(&config.server);
    Ok(config)
}

fn is_empty_list(value: &Value) -> bool {
    match value {
        Value::Array(items) => items.is_empty(),
        Value::Null => true,
        _ => false,
    }
}

/// Read a JSON or TOML config file into raw values.
///
/// Keys are lowercased and dashes become underscores; the file has always
/// spelled the token option in the singular.
pub fn read_config_file(path: &str) -> Result<RawValues, ConfigError> {
    let text = std::fs::read_to_string(path)
        .map_err(|_| ConfigError(format!("Config file {path} doesn't exist")))?;
    let value: Value = if path.ends_with(".toml") {
        let parsed: toml::Value = toml::from_str(&text)
            .map_err(|ex| ConfigError(format!("Cannot read option in {path}: {ex}")))?;
        serde_json::to_value(parsed)
            .map_err(|ex| ConfigError(format!("Cannot read option in {path}: {ex}")))?
    } else {
        serde_json::from_str(&text)
            .map_err(|ex| ConfigError(format!("Cannot read config file {path}: {ex}")))?
    };
    let Value::Object(map) = value else {
        return Err(ConfigError(format!("Config file {path} must contain an object")));
    };
    let mut values = RawValues::new();
    for (key, value) in map {
        let key = key.to_lowercase().replace('-', "_");
        let key = if key == "access_token" {
            "access_tokens".to_string()
        } else {
            key
        };
        values.insert(key, value);
    }
    Ok(values)
}

/// Read the `ff_*` environment variables into raw values.
///
/// Any variable starting with `ff_access_token` supplies an access token.
pub fn read_environ(environ: &[(String, String)]) -> RawValues {
    let mut values = RawValues::new();
    let mut tokens: Vec<String> = Vec::new();
    for (name, value) in environ {
        let name = name.to_lowercase();
        if let Some(token) = name.strip_prefix(ACCESS_TOKEN_ENV_PREFIX) {
            let _ = token;
            tokens.push(value.clone());
        } else if let Some(option) = name.strip_prefix(ENV_PREFIX) {
            if is_known_option(option) {
                values.insert(option.to_string(), Value::String(value.clone()));
            }
        }
    }
    if !tokens.is_empty() {
        values.insert("access_tokens".to_string(), Value::Array(
            tokens.into_iter().map(Value::String).collect(),
        ));
    }
    values
}

fn is_known_option(name: &str) -> bool {
    matches!(
        name,
        "server"
            | "access_tokens"
            | "reply_interval_in_hours"
            | "home_timeline_length"
            | "user"
            | "max_followings"
            | "max_followers"
            | "max_follow_requests"
            | "max_bookmarks"
            | "max_favourites"
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
            | "instance_blocklist"
            | "state_dir"
            | "lock_file"
            | "lock_hours"
            | "on_start"
            | "on_done"
            | "on_fail"
            | "log_level"
            | "log_format"
    )
}

fn apply(config: &mut Config, values: &RawValues) -> Result<(), ConfigError> {
    for (name, value) in values {
        let result = match name.as_str() {
            "server" => coerce_string(value).map(|v| config.server = v),
            "access_tokens" => coerce_list(value).map(|v| config.access_tokens = v),
            "reply_interval_in_hours" => coerce_int(value).map(|v| config.reply_interval_in_hours = v),
            "home_timeline_length" => coerce_int(value).map(|v| config.home_timeline_length = v),
            "user" => coerce_string(value).map(|v| config.user = v),
            "max_followings" => coerce_int(value).map(|v| config.max_followings = v),
            "max_followers" => coerce_int(value).map(|v| config.max_followers = v),
            "max_follow_requests" => coerce_int(value).map(|v| config.max_follow_requests = v),
            "max_bookmarks" => coerce_int(value).map(|v| config.max_bookmarks = v),
            "max_favourites" => coerce_int(value).map(|v| config.max_favourites = v),
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
            "respect_user_opt_out" => {
                coerce_bool(value).map(|v| config.respect_user_opt_out = v)
            }
            "remember_users_for_hours" => {
                coerce_int(value).map(|v| config.remember_users_for_hours = v)
            }
            "remember_hosts_for_days" => {
                coerce_int(value).map(|v| config.remember_hosts_for_days = v)
            }
            "http_timeout" => coerce_int(value).map(|v| config.http_timeout = v),
            "instance_blocklist" => coerce_list(value).map(|v| config.instance_blocklist = v),
            "state_dir" => coerce_path(value).map(|v| config.state_dir = v),
            "lock_file" => coerce_optional_path(value).map(|v| config.lock_file = v),
            "lock_hours" => coerce_int(value).map(|v| config.lock_hours = v),
            "on_start" => coerce_optional_string(value).map(|v| config.on_start = v),
            "on_done" => coerce_optional_string(value).map(|v| config.on_done = v),
            "on_fail" => coerce_optional_string(value).map(|v| config.on_fail = v),
            "log_level" => coerce_string(value).map(|v| config.log_level = v),
            "log_format" => coerce_string(value).map(|v| config.log_format = v),
            // unknown options are ignored, as in the Python original
            _ => Ok(()),
        };
        result.map_err(|ex| ConfigError(format!("Cannot read option {name}: {ex}")))?;
    }
    Ok(())
}

type CoerceResult<T> = Result<T, String>;

fn coerce_bool(value: &Value) -> CoerceResult<bool> {
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
                Err(format!("{text:?} is not a yes/no value"))
            }
        }
        _ => Err(format!("{value} is not a yes/no value")),
    }
}

fn coerce_int(value: &Value) -> CoerceResult<i64> {
    match value {
        Value::Number(n) => n.as_i64().ok_or_else(|| format!("{value} is not an integer")),
        Value::Bool(b) => Ok(*b as i64),
        Value::String(text) => text
            .trim()
            .parse::<i64>()
            .map_err(|_| format!("{text:?} is not an integer")),
        _ => Err(format!("{value} is not an integer")),
    }
}

fn coerce_string(value: &Value) -> CoerceResult<String> {
    match value {
        Value::String(s) => Ok(s.clone()),
        Value::Number(n) => Ok(n.to_string()),
        Value::Bool(b) => Ok(b.to_string()),
        _ => Err(format!("{value} is not a string")),
    }
}

fn coerce_optional_string(value: &Value) -> CoerceResult<Option<String>> {
    match value {
        Value::Null => Ok(None),
        other => coerce_string(other).map(Some),
    }
}

fn coerce_path(value: &Value) -> CoerceResult<PathBuf> {
    match value {
        Value::String(s) => Ok(PathBuf::from(s)),
        _ => Err(format!("{value} is not a path")),
    }
}

fn coerce_optional_path(value: &Value) -> CoerceResult<Option<PathBuf>> {
    match value {
        Value::Null => Ok(None),
        other => coerce_path(other).map(Some),
    }
}

fn coerce_list(value: &Value) -> CoerceResult<Vec<String>> {
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
                _ => Err(format!("{item} is not a string")),
            })
            .collect(),
        _ => Err(format!("{value} is not a list of values")),
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
        assert!(build_config(RawValues::new(), RawValues::new(), RawValues::new()).is_err());
        assert!(build_config(values(vec![("server", json!("x"))]), RawValues::new(), RawValues::new()).is_err());
        assert!(build_config(minimal(), RawValues::new(), RawValues::new()).is_ok());
    }

    #[test]
    fn later_sources_win() {
        let file = values(vec![
            ("server", json!("file.example")),
            ("access_tokens", json!(["filetoken"])),
            ("max_followers", json!(5)),
        ]);
        let environ = values(vec![("max_followers", json!("7"))]);
        let cli = values(vec![("max_followers", json!("9"))]);
        let config = build_config(file, environ, cli).unwrap();
        assert_eq!(config.server, "file.example");
        assert_eq!(config.access_tokens, vec!["filetoken".to_string()]);
        assert_eq!(config.max_followers, 9);
    }

    #[test]
    fn server_is_normalized() {
        let mut raw = minimal();
        raw.insert("server".to_string(), json!("https://mstdn.thms.uk/"));
        let config = build_config(raw, RawValues::new(), RawValues::new()).unwrap();
        assert_eq!(config.server, "mstdn.thms.uk");
    }

    #[test]
    fn bool_coercion_matches_python() {
        for truthy in ["1", "true", "yes", "on", " TRUE "] {
            let config = build_config(
                values(vec![
                    ("server", json!("x")),
                    ("access_tokens", json!(["t"])),
                    ("from_lists", json!(truthy)),
                ]),
                RawValues::new(),
                RawValues::new(),
            )
            .unwrap();
            assert!(config.from_lists, "{truthy}");
        }
        for falsy in ["0", "false", "no", "off", ""] {
            let config = build_config(
                values(vec![
                    ("server", json!("x")),
                    ("access_tokens", json!(["t"])),
                    ("from_lists", json!(falsy)),
                ]),
                RawValues::new(),
                RawValues::new(),
            )
            .unwrap();
            assert!(!config.from_lists, "{falsy}");
        }
        assert!(build_config(
            values(vec![
                ("server", json!("x")),
                ("access_tokens", json!(["t"])),
                ("from_lists", json!("maybe")),
            ]),
            RawValues::new(),
            RawValues::new(),
        )
        .is_err());
    }

    #[test]
    fn defaults_match_python() {
        let config = build_config(minimal(), RawValues::new(), RawValues::new()).unwrap();
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
        assert_eq!(config.log_level, "DEBUG");
        assert_eq!(config.log_format, "%(asctime)s: %(message)s");
        assert_eq!(config.lock_path(), config.state_dir.join("lock.lock"));
    }

    #[test]
    fn list_options_accept_comma_strings_and_arrays() {
        let config = build_config(
            values(vec![
                ("server", json!("x")),
                ("access_tokens", json!(["t"])),
                ("instance_blocklist", json!("a.example, b.example")),
            ]),
            RawValues::new(),
            RawValues::new(),
        )
        .unwrap();
        assert_eq!(config.instance_blocklist, vec!["a.example", "b.example"]);

        let config = build_config(
            values(vec![
                ("server", json!("x")),
                ("access_tokens", json!(["t"])),
                ("instance_blocklist", json!(["a.example", "b.example"])),
            ]),
            RawValues::new(),
            RawValues::new(),
        )
        .unwrap();
        assert_eq!(config.instance_blocklist, vec!["a.example", "b.example"]);
    }

    #[test]
    fn unknown_options_are_ignored() {
        let config = build_config(
            values(vec![
                ("server", json!("x")),
                ("access_tokens", json!(["t"])),
                ("nonsense", json!(1)),
            ]),
            RawValues::new(),
            RawValues::new(),
        )
        .unwrap();
        assert_eq!(config.server, "x");
    }

    #[test]
    fn singular_access_token_key_is_accepted() {
        let raw = read_config_file_bytes(r#"{"server": "x", "access_token": "tok"}"#).unwrap();
        let config = build_config(raw, RawValues::new(), RawValues::new()).unwrap();
        assert_eq!(config.access_tokens, vec!["tok".to_string()]);
    }

    #[test]
    fn keys_are_lowercased_and_dashes_become_underscores() {
        let raw = read_config_file_bytes(r#"{"SERVER": "x", "Access-Token": ["t"], "Max-Followers": 3}"#).unwrap();
        let config = build_config(raw, RawValues::new(), RawValues::new()).unwrap();
        assert_eq!(config.server, "x");
        assert_eq!(config.max_followers, 3);
    }

    #[test]
    fn toml_config_file_is_accepted() {
        let dir = std::env::temp_dir().join(format!("ff-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        std::fs::write(&path, "server = \"x\"\naccess-token = \"tok\"\nmax-followers = 4\n").unwrap();
        let raw = read_config_file(path.to_str().unwrap()).unwrap();
        let config = build_config(raw, RawValues::new(), RawValues::new()).unwrap();
        assert_eq!(config.server, "x");
        assert_eq!(config.access_tokens, vec!["tok".to_string()]);
        assert_eq!(config.max_followers, 4);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn environ_reads_ff_prefixed_variables() {
        let environ = vec![
            ("FF_SERVER".to_string(), "env.example".to_string()),
            ("ff_max_followers".to_string(), "12".to_string()),
            ("ff_access_token_one".to_string(), "tok1".to_string()),
            ("ff_access_token_two".to_string(), "tok2".to_string()),
            ("ff_unknown_option".to_string(), "x".to_string()),
            ("unrelated".to_string(), "y".to_string()),
        ];
        let raw = read_environ(&environ);
        assert_eq!(raw.get("server"), Some(&json!("env.example")));
        assert_eq!(raw.get("max_followers"), Some(&json!("12")));
        assert_eq!(
            raw.get("access_tokens"),
            Some(&json!(["tok1", "tok2"]))
        );
        assert!(!raw.contains_key("unknown_option"));
        assert!(!raw.contains_key("unrelated"));
    }

    fn read_config_file_bytes(text: &str) -> Result<RawValues, ConfigError> {
        let dir = std::env::temp_dir().join(format!("ff-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.json");
        std::fs::write(&path, text).unwrap();
        let raw = read_config_file(path.to_str().unwrap());
        std::fs::remove_dir_all(&dir).ok();
        raw
    }
}
