//! The command line: run once (the default), or serve the web UI.
//!
//! The flags mirror the Python original's argparse setup: every option takes
//! a value spelled the same way in the database and the `ff_*` environment,
//! and `--access-token` may be given several times.

use std::process::ExitCode;

use clap::{Args, Parser, Subcommand};
use serde_json::Value;

use fedifetcher::config::{RawValues, build_config, build_config_lenient, read_environ};
use fedifetcher::{db, logging, run, web};

#[derive(Parser)]
#[command(
    name = "fedifetcher",
    version,
    about = "Pull missing posts and profiles into your Mastodon instance",
    long_about = "Pull missing posts and profiles into your Mastodon instance.\n\n\
        With no subcommand, runs once. Configuration lives in the database (edited \
        through the web UI's config page); the ff_* environment variables and the \
        command line override the stored configuration, in increasing order of precedence."
)]
struct Cli {
    #[command(flatten)]
    config: ConfigArgs,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Run FediFetcher once, then exit (the default when no subcommand is given)
    Run {
        #[command(flatten)]
        config: ConfigArgs,
    },
    /// Serve the web UI (dashboard, run trigger, hosts, logs, config)
    Serve {
        #[command(flatten)]
        config: ConfigArgs,
        /// The address to listen on
        #[arg(long = "bind", default_value = "127.0.0.1")]
        bind: String,
        /// The port to listen on
        #[arg(long = "port", default_value_t = 8080)]
        port: u16,
    },
}

/// Every configuration option as a command line flag. Values stay as text
/// here and are coerced by `build_config`, so the command line, the
/// environment and the database are all read the same way.
#[derive(Args, Clone, Default)]
struct ConfigArgs {
    /// Required: The name of your server (e.g. `mstdn.thms.uk`)
    #[arg(long = "server")]
    server: Option<String>,

    /// Required: The access token can be generated at https://<server>/settings/applications, and must have read:search, read:statuses and admin:read:accounts scopes. You can supply this multiple times, if you want to run it for multiple users.
    #[arg(long = "access-token", value_name = "ACCESS_TOKEN")]
    access_tokens: Vec<String>,

    /// Fetch remote replies to posts that have received replies from users on your own instance in this period
    #[arg(long = "reply-interval-in-hours")]
    reply_interval_in_hours: Option<String>,

    /// Look for replies to posts in the API-Key owner's home timeline, up to this many posts
    #[arg(long = "home-timeline-length")]
    home_timeline_length: Option<String>,

    /// Optional. Use together with --max-followings or --max-followers to backfill a specific user's followings/followers. If omitted, we use the account that owns the access token.
    #[arg(long = "user")]
    user: Option<String>,

    /// Backfill posts for new accounts followed by --user. We'll backfill at most this many followings' posts
    #[arg(long = "max-followings")]
    max_followings: Option<String>,

    /// Backfill posts for new accounts following --user. We'll backfill at most this many followers' posts
    #[arg(long = "max-followers")]
    max_followers: Option<String>,

    /// Backfill posts of the API key owners pending follow requests. We'll backfill at most this many requester's posts
    #[arg(long = "max-follow-requests")]
    max_follow_requests: Option<String>,

    /// Fetch remote replies to the API key owners Bookmarks. We'll fetch replies to at most this many bookmarks
    #[arg(long = "max-bookmarks")]
    max_bookmarks: Option<String>,

    /// Fetch remote replies to the API key owners Favourites. We'll fetch replies to at most this many favourites
    #[arg(long = "max-favourites")]
    max_favourites: Option<String>,

    /// Backfill accounts of anyone appearing in your notifications, during the last hours
    #[arg(long = "from-notifications")]
    from_notifications: Option<String>,

    /// Set to `1` to fetch missing replies and/or backfill account from your lists. This is disabled by default.
    #[arg(long = "from-lists")]
    from_lists: Option<String>,

    /// Determines how many posts we'll fetch replies for in each list. This will be ignored, unless you also provide `from-lists = 1`. Set to `0` if you only want to backfill profiles in lists.
    #[arg(long = "max-list-length")]
    max_list_length: Option<String>,

    /// Determines how many accounts we'll backfill for in each list. This will be ignored, unless you also provide `from-lists = 1`. Set to `0` if you only want to fetch replies in lists.
    #[arg(long = "max-list-accounts")]
    max_list_accounts: Option<String>,

    /// How many posts to fetch from each account we backfill. Anything above the 40 we fetch by default means more requests to the remote server, and more requests to your own server to pull each post in, so raising this will make FediFetcher considerably slower.
    #[arg(long = "max-posts-per-account")]
    max_posts_per_account: Option<String>,

    /// If enabled, we'll fetch remote replies when backfilling profiles. Set to `0` to disable.
    #[arg(long = "backfill-with-context")]
    backfill_with_context: Option<String>,

    /// If enabled, we'll backfill any mentioned users when fetching remote replies to timeline posts. Set to `0` to disable.
    #[arg(long = "backfill-mentioned-users")]
    backfill_mentioned_users: Option<String>,

    /// Respect robots.txt when fetching from remote servers (default: enabled)
    #[arg(long = "respect-robots-txt")]
    respect_robots_txt: Option<String>,

    /// Skip accounts that have opted out (nobot tag, or not indexable or discoverable) when backfilling (default: enabled)
    #[arg(long = "respect-user-opt-out")]
    respect_user_opt_out: Option<String>,

    /// How long to remember users that you aren't following for, before trying to backfill them again.
    #[arg(long = "remember-users-for-hours")]
    remember_users_for_hours: Option<String>,

    /// How long to remember host info for, before checking again.
    #[arg(long = "remember-hosts-for-days")]
    remember_hosts_for_days: Option<String>,

    /// The timeout for any HTTP requests to your own, or other instances.
    #[arg(long = "http-timeout")]
    http_timeout: Option<String>,

    /// A comma-separated list of seconds to wait between retries when a server rate limits us (HTTP 429): one entry per retry step, tried in order. When the list runs out, the request fails. Default: 0.5,2,8,32,128
    #[arg(long = "rate-limit-cooldown")]
    rate_limit_cooldown: Option<String>,

    /// A comma-separated array of instances that FediFetcher should never try to connect to
    #[arg(long = "instance-blocklist")]
    instance_blocklist: Option<String>,

    /// Directory to store persistent files and possibly lock file. Defaults to ./artifacts if that directory exists, and to $XDG_STATE_HOME/fedifetcher (usually ~/.local/state/fedifetcher) otherwise.
    #[arg(long = "state-dir")]
    state_dir: Option<String>,

    /// Location of the lock file
    #[arg(long = "lock-file")]
    lock_file: Option<String>,

    /// The lock timeout in hours.
    #[arg(long = "lock-hours")]
    lock_hours: Option<String>,

    /// Provide a url that will be pinged when processing is starting. You can use this for 'dead man switch' monitoring of your task
    #[arg(long = "on-start")]
    on_start: Option<String>,

    /// Provide a url that will be pinged when processing has completed. You can use this for 'dead man switch' monitoring of your task
    #[arg(long = "on-done")]
    on_done: Option<String>,

    /// Provide a url that will be pinged when processing has failed. You can use this for 'dead man switch' monitoring of your task
    #[arg(long = "on-fail")]
    on_fail: Option<String>,

    /// Severity of events to log (DEBUG|INFO|WARNING|ERROR|CRITICAL)
    #[arg(long = "log-level")]
    log_level: Option<String>,

    /// Specify the log format
    #[arg(long = "log-format")]
    log_format: Option<String>,
}

enum Mode {
    Run,
    Serve { bind: String, port: u16 },
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();

    // a subcommand's flags win over the top level's, as later sources do
    let (cli_values, mode) = match cli.command {
        Some(Command::Run { config }) => {
            let raw = args_to_raw(&config);
            let top = args_to_raw(&cli.config);
            (merge(top, raw), Mode::Run)
        }
        Some(Command::Serve {
            config,
            bind,
            port,
        }) => {
            let raw = args_to_raw(&config);
            let top = args_to_raw(&cli.config);
            (merge(top, raw), Mode::Serve { bind, port })
        }
        None => {
            let raw = args_to_raw(&cli.config);
            (raw, Mode::Run)
        }
    };

    let environ = read_environ(&std::env::vars().collect::<Vec<_>>());

    // The database lives in the state directory, so bootstrap a config from
    // the environment and the command line to find it.
    let bootstrap =
        match build_config_lenient(RawValues::new(), environ.clone(), cli_values.clone()) {
            Ok(config) => config,
            Err(ex) => return config_error(ex),
        };
    let db = match db::open(&bootstrap).await {
        Ok(db) => db,
        Err(ex) => {
            eprintln!("CRITICAL:FediFetcher:{ex}");
            return ExitCode::from(1);
        }
    };

    // The database holds the configuration; the web UI's config page edits it
    // there.
    let stored = match db::load_config(&db).await {
        Ok(values) => values,
        Err(ex) => {
            eprintln!("CRITICAL:FediFetcher:{ex}");
            return ExitCode::from(1);
        }
    };

    // Serving only needs a config the page can edit; running needs a server
    // and an access token.
    let config = match &mode {
        Mode::Run => build_config(stored, environ, cli_values),
        Mode::Serve { .. } => build_config_lenient(stored, environ, cli_values),
    };
    let config = match config {
        Ok(config) => config,
        Err(ex) => return config_error(ex),
    };

    if let Err(ex) = logging::setup_logging(&config) {
        return config_error(ex);
    }

    // Every log line is written to the database, which the web UI reads for
    // the logs page and the run history.
    if let Err(ex) = logging::setup_log_db(db.clone()).await {
        eprintln!("CRITICAL:FediFetcher:{ex}");
        return ExitCode::from(1);
    }

    match mode {
        Mode::Run => {
            let result = run::run_once(config, run::CancelToken::new(), &db).await;
            // wait for the log lines already received to reach the database
            logging::shutdown_log_db().await;
            match result {
                Ok(_) => ExitCode::SUCCESS,
                Err(_) => ExitCode::from(1),
            }
        }
        Mode::Serve { bind, port } => {
            match web::serve(config, &bind, port, db).await {
                Ok(()) => ExitCode::SUCCESS,
                Err(ex) => {
                    eprintln!("ERROR:FediFetcher:{ex}");
                    ExitCode::from(1)
                }
            }
        }
    }
}

/// The Python original logs a config problem as critical and exits 1.
fn config_error(ex: fedifetcher::error::ConfigError) -> ExitCode {
    eprintln!("CRITICAL:FediFetcher:{ex}");
    ExitCode::from(1)
}

/// The raw values of one set of command line flags: only what was given.
fn args_to_raw(args: &ConfigArgs) -> RawValues {
    let mut raw = RawValues::new();
    let mut put = |key: &str, value: &Option<String>| {
        if let Some(value) = value {
            raw.insert(key.to_string(), Value::String(value.clone()));
        }
    };
    put("server", &args.server);
    put("reply_interval_in_hours", &args.reply_interval_in_hours);
    put("home_timeline_length", &args.home_timeline_length);
    put("user", &args.user);
    put("max_followings", &args.max_followings);
    put("max_followers", &args.max_followers);
    put("max_follow_requests", &args.max_follow_requests);
    put("max_bookmarks", &args.max_bookmarks);
    put("max_favourites", &args.max_favourites);
    put("from_notifications", &args.from_notifications);
    put("from_lists", &args.from_lists);
    put("max_list_length", &args.max_list_length);
    put("max_list_accounts", &args.max_list_accounts);
    put("max_posts_per_account", &args.max_posts_per_account);
    put("backfill_with_context", &args.backfill_with_context);
    put("backfill_mentioned_users", &args.backfill_mentioned_users);
    put("respect_robots_txt", &args.respect_robots_txt);
    put("respect_user_opt_out", &args.respect_user_opt_out);
    put("remember_users_for_hours", &args.remember_users_for_hours);
    put("remember_hosts_for_days", &args.remember_hosts_for_days);
    put("http_timeout", &args.http_timeout);
    put("rate_limit_cooldown", &args.rate_limit_cooldown);
    put("instance_blocklist", &args.instance_blocklist);
    put("state_dir", &args.state_dir);
    put("lock_file", &args.lock_file);
    put("lock_hours", &args.lock_hours);
    put("on_start", &args.on_start);
    put("on_done", &args.on_done);
    put("on_fail", &args.on_fail);
    put("log_level", &args.log_level);
    put("log_format", &args.log_format);
    if !args.access_tokens.is_empty() {
        raw.insert(
            "access_tokens".to_string(),
            Value::Array(
                args.access_tokens
                    .iter()
                    .map(|token| Value::String(token.clone()))
                    .collect(),
            ),
        );
    }
    raw
}

/// `over`'s values win, key by key.
fn merge(base: RawValues, over: RawValues) -> RawValues {
    let mut merged = base;
    for (key, value) in over {
        merged.insert(key, value);
    }
    merged
}
