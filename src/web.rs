//! A server-rendered web UI: dashboard, run trigger, hosts, logs and an
//! editable config page that writes back to the config file. No JavaScript
//! anywhere — every page is HTML from leptos SSR, and the only interactivity
//! is a form POST that redirects back.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use axum::body::Bytes;
use axum::extract::{Query, State as AxumState};
use axum::http::StatusCode;
use axum::response::{Html, Redirect};
use axum::routing::{get, post};
use axum::Router;
use leptos::prelude::*;
use regex::Regex;
use serde_json::Value;
use sqlx::SqlitePool;
use time::OffsetDateTime;
use tokio::sync::Mutex as AsyncMutex;

use crate::api::mastodon::masked;
use crate::config::{Config, RawValues, is_known_option, option_description, write_config_file};
use crate::db::{self, LogOrder, LogRow, LogSort, RunRow};
use crate::debug::{self, DebugOutcome};
use crate::http::HttpClient;
use crate::run::{CancelToken, run_once};
use crate::store::StateStore;
use crate::timeutil;
use crate::VERSION;

/// The most runs remembered for the dashboard's history.
const HISTORY_CAPACITY: usize = 50;

/// The most log lines shown on /logs.
const LOG_LINES_SHOWN: usize = 200;

struct AppState {
    /// The effective config; replaced when the config page saves.
    config: Mutex<Config>,
    /// Where the config page writes; TOML, or JSON when it ends in .json.
    config_file: PathBuf,
    /// The database the log lines and run history are written to, and the
    /// posts found private are recorded in.
    db: SqlitePool,
    ui: Mutex<UiState>,
    /// Held for the duration of a web-triggered run, so two clicks cannot
    /// overlap; the lock file keeps other processes out.
    run_guard: Arc<AsyncMutex<()>>,
    /// Set by the stop button; the running run checks it between tasks and
    /// items, and finishes early when it is set.
    cancel: CancelToken,
}

struct UiState {
    status: RunStatus,
}

enum RunStatus {
    Idle,
    Running { since: OffsetDateTime },
}

/// Serve the web UI until the process is stopped. The config page writes
/// back to `config_file`.
pub async fn serve(
    config: Config,
    config_file: &str,
    bind: &str,
    port: u16,
    db: SqlitePool,
) -> std::io::Result<()> {
    let app = app(config, PathBuf::from(config_file), db);

    let addr: std::net::SocketAddr = format!("{bind}:{port}")
        .parse()
        .map_err(|ex| std::io::Error::new(std::io::ErrorKind::InvalidInput, ex))?;
    tracing::info!("FediFetcher v{VERSION} web UI listening on http://{addr} (config: {config_file})");
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await
}

/// Build the web UI's router: the dashboard, the run trigger, hosts, logs,
/// the editable config page, the debug page, and the health check.
pub fn app(config: Config, config_file: PathBuf, db: SqlitePool) -> Router {
    let state = Arc::new(AppState {
        config: Mutex::new(config),
        config_file,
        db,
        ui: Mutex::new(UiState {
            status: RunStatus::Idle,
        }),
        run_guard: Arc::new(AsyncMutex::new(())),
        cancel: CancelToken::new(),
    });

    Router::new()
        .route("/", get(dashboard))
        .route("/run", post(run))
        .route("/stop", post(stop))
        .route("/hosts", get(hosts))
        .route("/logs", get(logs))
        .route("/logs/clear", post(clear_logs))
        .route("/config", get(config_page).post(save_config))
        .route("/debug", get(debug_page).post(debug_post))
        .route("/healthz", get(healthz))
        .with_state(state)
}

async fn dashboard(AxumState(state): AxumState<Arc<AppState>>) -> Html<String> {
    tracing::debug!("GET /");
    let history = match db::recent_runs(&state.db, HISTORY_CAPACITY).await {
        Ok(runs) => runs,
        Err(ex) => {
            tracing::warn!("Could not read the run history: {ex}");
            Vec::new()
        }
    };
    render(move || view! { <Dashboard state=state history=history /> })
}

/// Which column of the hosts table the /hosts page sorts by.
#[derive(Clone, Copy, PartialEq, Eq)]
enum HostSort {
    Host,
    Software,
    Version,
    Apis,
    LastChecked,
}

impl HostSort {
    /// The /hosts query string's name for the column.
    fn param(self) -> &'static str {
        match self {
            HostSort::Host => "host",
            HostSort::Software => "software",
            HostSort::Version => "version",
            HostSort::Apis => "apis",
            HostSort::LastChecked => "last_checked",
        }
    }

    /// The column a /hosts query string asks for, defaulting to the host.
    fn parse(param: Option<&str>) -> Self {
        match param {
            Some("software") => HostSort::Software,
            Some("version") => HostSort::Version,
            Some("apis") => HostSort::Apis,
            Some("last_checked") => HostSort::LastChecked,
            _ => HostSort::Host,
        }
    }
}

/// The /hosts page's query string: which column to sort by, and which way.
/// Unknown values fall back to the defaults, hosts alphabetically.
#[derive(serde::Deserialize)]
struct HostsQuery {
    sort: Option<String>,
    dir: Option<String>,
}

async fn hosts(
    AxumState(state): AxumState<Arc<AppState>>,
    Query(query): Query<HostsQuery>,
) -> Html<String> {
    tracing::debug!("GET /hosts");
    let sort = HostSort::parse(query.sort.as_deref());
    let order = match query.dir.as_deref() {
        Some("desc") => LogOrder::Desc,
        _ => LogOrder::Asc,
    };
    render(move || view! { <HostsPage state=state sort=sort order=order /> })
}

/// The /logs page's query string: which column to sort by, and which way.
/// Unknown values fall back to the defaults, newest first.
#[derive(serde::Deserialize)]
struct LogsQuery {
    sort: Option<String>,
    dir: Option<String>,
}

async fn logs(
    AxumState(state): AxumState<Arc<AppState>>,
    Query(query): Query<LogsQuery>,
) -> Html<String> {
    tracing::debug!("GET /logs");
    let sort = LogSort::parse(query.sort.as_deref());
    let order = match query.dir.as_deref() {
        Some("asc") => LogOrder::Asc,
        _ => LogOrder::Desc,
    };
    let rows = match db::recent_logs(&state.db, LOG_LINES_SHOWN, sort, order).await {
        Ok(rows) => rows,
        Err(ex) => {
            tracing::warn!("Could not read the log: {ex}");
            Vec::new()
        }
    };
    render(move || view! { <LogsPage rows=rows sort=sort order=order /> })
}

/// Empty the log the /logs page shows, and redirect back to it.
async fn clear_logs(AxumState(state): AxumState<Arc<AppState>>) -> Redirect {
    tracing::info!("POST /logs/clear: clearing the log");
    match db::clear_logs(&state.db).await {
        Ok(()) => tracing::info!("The log is cleared"),
        Err(ex) => tracing::warn!("Could not clear the log: {ex}"),
    }
    Redirect::to("/logs")
}

async fn config_page(AxumState(state): AxumState<Arc<AppState>>) -> Html<String> {
    tracing::debug!("GET /config");
    render(move || view! { <ConfigPage state=state /> })
}

/// Save the config page's form: apply the fields to the effective config,
/// write it back to the config file, and redirect to the page.
///
/// The redirect is what makes this work without JavaScript: the POST answers
/// 303 See Other, and the browser follows it with a GET.
async fn save_config(
    AxumState(state): AxumState<Arc<AppState>>,
    body: Bytes,
) -> Result<Redirect, (StatusCode, Html<String>)> {
    let pairs: Vec<(String, String)> = url::form_urlencoded::parse(&body)
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect();

    let mut values = RawValues::new();
    let mut remove_tokens: Vec<usize> = Vec::new();
    let mut new_token = String::new();
    for (key, value) in &pairs {
        match key.as_str() {
            "remove_access_token" => {
                if let Ok(index) = value.parse::<usize>() {
                    remove_tokens.push(index);
                }
            }
            "new_access_token" => {
                let token = value.trim();
                if !token.is_empty() {
                    new_token = token.to_string();
                }
            }
            name if is_known_option(name) => {
                // an empty field for an optional option means "unset"
                let value = if value.is_empty() && is_optional_option(name) {
                    Value::Null
                } else {
                    Value::String(value.clone())
                };
                values.insert(name.to_string(), value);
            }
            _ => {}
        }
    }

    let mut config = state.config.lock().unwrap().clone();
    let before = config.to_raw_values();
    if let Err(ex) = config.apply_values(&values) {
        tracing::warn!("POST /config: not saved: {ex}");
        return Err(save_error(StatusCode::BAD_REQUEST, &ex.to_string()));
    }
    if !remove_tokens.is_empty() || !new_token.is_empty() {
        let mut tokens: Vec<String> = config
            .access_tokens
            .iter()
            .enumerate()
            .filter(|(index, _)| !remove_tokens.contains(index))
            .map(|(_, token)| token.clone())
            .collect();
        if !new_token.is_empty() {
            tokens.push(new_token);
        }
        config.access_tokens = tokens;
    }

    let config_file = state.config_file.to_string_lossy().into_owned();
    if let Err(ex) = write_config_file(&config_file, &config.to_raw_values()) {
        tracing::warn!("POST /config: not saved: {ex}");
        return Err(save_error(StatusCode::INTERNAL_SERVER_ERROR, &ex.to_string()));
    }
    let after = config.to_raw_values();
    // option names only: the values can hold access tokens
    let changed: Vec<String> = after
        .keys()
        .filter(|key| before.get(*key) != after.get(*key))
        .cloned()
        .collect();
    tracing::info!("Configuration saved to {config_file}");
    tracing::debug!(
        "POST /config: changed options: {}",
        if changed.is_empty() {
            "none".to_string()
        } else {
            changed.join(", ")
        }
    );
    *state.config.lock().unwrap() = config;
    Ok(Redirect::to("/config"))
}

/// Options whose empty form field means "unset".
fn is_optional_option(name: &str) -> bool {
    matches!(name, "lock_file" | "on_start" | "on_done" | "on_fail")
}

/// A failed save: the message, and a way back to the form.
fn save_error(status: StatusCode, message: &str) -> (StatusCode, Html<String>) {
    let message = message.to_string();
    let body = render(move || {
        view! {
            <Page title="Config".to_string()>
                <section>
                    <h2>"Could not save the configuration"</h2>
                    <p><code>{message}</code></p>
                    <p><a href="/config">"Back to the config page"</a></p>
                </section>
            </Page>
        }
    });
    (status, body)
}

async fn debug_page(AxumState(state): AxumState<Arc<AppState>>) -> Html<String> {
    tracing::debug!("GET /debug");
    render(move || view! { <DebugPage state=state result=None /> })
}

/// Run the debug tab's form, and render the page with the result. The
/// result is shown in the POST's own response: there is nowhere to
/// redirect to that would not lose it.
async fn debug_post(AxumState(state): AxumState<Arc<AppState>>, body: Bytes) -> Html<String> {
    let pairs: Vec<(String, String)> = url::form_urlencoded::parse(&body)
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect();
    let outcome = run_debug_action(&state, &pairs).await;
    match &outcome {
        DebugOutcome::Failed { message } => tracing::warn!("POST /debug: failed: {message}"),
        _ => tracing::debug!("POST /debug: succeeded"),
    }
    render(move || view! { <DebugPage state=state result=Some(outcome) /> })
}

/// Run one debug action against the configured server, with the first
/// access token.
async fn run_debug_action(state: &AppState, pairs: &[(String, String)]) -> DebugOutcome {
    let field = |name: &str| {
        pairs
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.trim().to_string())
            .unwrap_or_default()
    };
    let action = field("action");
    let description = match action.as_str() {
        "connect" => "test the connection".to_string(),
        "user" => format!("look up user {}", field("user")),
        "posts" => format!("fetch {} posts for {}", field("limit"), field("user")),
        other => format!("unknown action {other:?}"),
    };
    tracing::info!("POST /debug: {description}");
    let config = state.config.lock().unwrap().clone();
    if config.server.is_empty() {
        return DebugOutcome::Failed {
            message: "No server is configured — set one on the Config page.".to_string(),
        };
    }
    let Some(token) = config.access_tokens.first() else {
        return DebugOutcome::Failed {
            message: "No access token is configured — add one on the Config page.".to_string(),
        };
    };
    let token = token.clone();
    let base = format!("https://{}", config.server);
    let http = HttpClient::new(config);
    match action.as_str() {
        "connect" => match debug::test_connection(&base, &token, &http).await {
            Ok((instance, account)) => DebugOutcome::Connected { instance, account },
            Err(ex) => DebugOutcome::Failed {
                message: ex.to_string(),
            },
        },
        "user" => {
            let acct = field("user");
            if acct.is_empty() {
                return DebugOutcome::Failed {
                    message: "Enter an account to look up, e.g. user@example.social.".to_string(),
                };
            }
            match debug::lookup_account(&base, &acct, &token, &http).await {
                Ok(account) => DebugOutcome::User { acct, account },
                Err(ex) => DebugOutcome::Failed {
                    message: ex.to_string(),
                },
            }
        }
        "posts" => {
            let acct = field("user");
            if acct.is_empty() {
                return DebugOutcome::Failed {
                    message: "Enter an account to fetch posts for, e.g. user@example.social."
                        .to_string(),
                };
            }
            let limit = field("limit").parse::<i64>().unwrap_or(10).clamp(1, 40);
            match debug::account_posts(&base, &acct, limit, &token, &http).await {
                Ok((account, posts)) => DebugOutcome::Posts {
                    acct,
                    account,
                    posts,
                },
                Err(ex) => DebugOutcome::Failed {
                    message: ex.to_string(),
                },
            }
        }
        _ => DebugOutcome::Failed {
            message: format!("Unknown action {action:?}."),
        },
    }
}

async fn healthz() -> &'static str {
    "ok"
}

/// Trigger a run in the background, then redirect back to the dashboard.
///
/// The redirect is what makes this work without JavaScript: the POST answers
/// 303 See Other, and the browser follows it with a GET.
async fn run(AxumState(state): AxumState<Arc<AppState>>) -> Redirect {
    if let Ok(guard) = state.run_guard.clone().try_lock_owned() {
        tracing::info!("POST /run: run triggered from the web UI");
        state.cancel.reset();
        tokio::spawn(async move {
            let started = timeutil::now();
            {
                let mut ui = state.ui.lock().unwrap();
                ui.status = RunStatus::Running { since: started };
            }

            let config = state.config.lock().unwrap().clone();
            let cancel = state.cancel.clone();
            let outcome = run_once(config.clone(), cancel.clone(), &state.db).await;
            let finished = timeutil::now();
            let (outcome_label, message) = match &outcome {
                Ok(message) if cancel.is_cancelled() => ("stopped".to_string(), message.clone()),
                Ok(message) => ("ok".to_string(), message.clone()),
                Err(ex) => ("error".to_string(), ex.to_string()),
            };
            let record = RunRow {
                started: timeutil::fmt_state_ts(started),
                finished: timeutil::fmt_state_ts(finished),
                outcome: outcome_label,
                message,
            };
            if let Err(ex) = db::insert_run(&state.db, &record).await {
                tracing::warn!("Could not record the run in the database: {ex}");
            }

            let mut ui = state.ui.lock().unwrap();
            ui.status = RunStatus::Idle;
            drop(ui);
            drop(guard);
        });
    } else {
        tracing::debug!("POST /run: a run is already in progress; trigger ignored");
    }
    Redirect::to("/")
}

/// Ask the running FediFetcher to stop: set the token the run checks between
/// tasks and items, then redirect back to the dashboard. Stopping an idle UI
/// is harmless — the next run clears the flag first.
async fn stop(AxumState(state): AxumState<Arc<AppState>>) -> Redirect {
    let running = matches!(state.ui.lock().unwrap().status, RunStatus::Running { .. });
    state.cancel.cancel();
    if running {
        tracing::info!("POST /stop: stop requested for the running FediFetcher run");
    } else {
        tracing::debug!("POST /stop: no run in progress; nothing to stop");
    }
    Redirect::to("/")
}

/// Render a view to a full HTML document. SSR only: the result is static
/// HTML, with no JavaScript to hydrate it.
fn render<F, N>(make: F) -> Html<String>
where
    F: FnOnce() -> N,
    N: IntoView,
{
    Html(format!("<!DOCTYPE html>{}", make().to_html()))
}

/// One row of the config form: a text input, with the option's description
/// as a hover tooltip (a title attribute — no JavaScript needed).
fn text_row(name: &str, value: &str) -> AnyView {
    let tooltip = option_description(name).map(str::to_string);
    view! {
        <tr>
            <td><code>{name.to_string()}</code></td>
            <td><input type="text" name={name.to_string()} value={value.to_string()} title=tooltip/></td>
        </tr>
    }
    .into_any()
}

/// One row of the config form: a number input, with the option's description
/// as a hover tooltip.
fn int_row(name: &str, value: i64) -> AnyView {
    let tooltip = option_description(name).map(str::to_string);
    view! {
        <tr>
            <td><code>{name.to_string()}</code></td>
            <td><input type="number" name={name.to_string()} value={value.to_string()} title=tooltip/></td>
        </tr>
    }
    .into_any()
}

/// One row of the config form: a yes/no select, which always submits a value,
/// with the option's description as a hover tooltip.
fn bool_row(name: &str, value: bool) -> AnyView {
    let tooltip = option_description(name).map(str::to_string);
    let (yes, no) = if value {
        (
            view! { <option value="1" selected="selected">"yes"</option> }.into_any(),
            view! { <option value="0">"no"</option> }.into_any(),
        )
    } else {
        (
            view! { <option value="1">"yes"</option> }.into_any(),
            view! { <option value="0" selected="selected">"no"</option> }.into_any(),
        )
    };
    view! {
        <tr>
            <td><code>{name.to_string()}</code></td>
            <td>
                <select name={name.to_string()} title=tooltip>
                    {yes}
                    {no}
                </select>
            </td>
        </tr>
    }
    .into_any()
}

/// The access tokens row: the current tokens masked, each with a remove
/// checkbox, plus a password field to add one. The tokens themselves are
/// never sent to the browser.
fn tokens_row(config: &Config) -> AnyView {
    let items: Vec<AnyView> = config
        .access_tokens
        .iter()
        .enumerate()
        .map(|(index, token)| {
            let masked = masked(token);
            view! {
                <li>
                    <code>{masked}</code>
                    " "
                    <label>
                        <input type="checkbox" name="remove_access_token" value={index.to_string()}/>
                        "remove"
                    </label>
                </li>
            }
            .into_any()
        })
        .collect();
    view! {
        <tr>
            <td><code>"access_tokens"</code></td>
            <td>
                {if items.is_empty() {
                    view! { <p class="muted">"No access tokens set."</p> }.into_any()
                } else {
                    view! { <ul class="tokens">{items}</ul> }.into_any()
                }}
                <p>
                    <input type="password" name="new_access_token" placeholder="new access token"
                        autocomplete="new-password"
                        title=option_description("access_tokens").map(str::to_string)/>
                </p>
                <p class="muted">"Tokens are never shown in full; add one, or tick remove."</p>
            </td>
        </tr>
    }
    .into_any()
}

/// The log level row: a select of the levels `setup_logging` accepts.
fn log_level_row(value: &str) -> AnyView {
    let levels = ["DEBUG", "INFO", "WARNING", "ERROR", "CRITICAL"];
    let tooltip = option_description("log_level").map(str::to_string);
    view! {
        <tr>
            <td><code>"log_level"</code></td>
            <td>
                <select name="log_level" title=tooltip>
                    {levels
                        .iter()
                        .map(|level| {
                            if *level == value {
                                view! { <option value={level.to_string()} selected="selected">{level.to_string()}</option> }.into_any()
                            } else {
                                view! { <option value={level.to_string()}>{level.to_string()}</option> }.into_any()
                            }
                        })
                        .collect::<Vec<_>>()}
                </select>
            </td>
        </tr>
    }
    .into_any()
}

const CSS: &str = r#"
:root { color-scheme: light dark; }
body { font-family: system-ui, sans-serif; margin: 0; }
header { display: flex; gap: 1.5rem; align-items: baseline; padding: 0.75rem 1.5rem; border-bottom: 1px solid #8884; flex-wrap: wrap; }
header h1 { font-size: 1.1rem; margin: 0; }
header h1 a { color: inherit; text-decoration: none; }
.version { color: #888; font-weight: normal; }
nav { display: flex; gap: 1rem; }
nav a { color: #06c; }
main { padding: 1rem 1.5rem; max-width: 64rem; }
section { margin-bottom: 1.75rem; }
h2 { font-size: 1rem; }
table { border-collapse: collapse; width: 100%; }
th, td { text-align: left; padding: 0.3rem 0.6rem; border-bottom: 1px solid #8884; vertical-align: top; }
th { font-weight: 600; }
code, pre { font-family: ui-monospace, monospace; font-size: 0.85em; }
pre { background: #8881; padding: 0.75rem; overflow-x: auto; }
button { font-size: 1rem; padding: 0.4rem 1rem; }
input, select { font: inherit; padding: 0.2rem 0.4rem; }
td input, td select { width: 100%; max-width: 30rem; box-sizing: border-box; }
form input[type="text"] { width: 22rem; max-width: 100%; }
form input[type="number"] { width: 6rem; }
pre { white-space: pre-wrap; }
ul.tokens { margin: 0.2rem 0; padding-left: 1.2rem; }
ul.tokens label { color: #888; }
.badge { padding: 0.1rem 0.5rem; border-radius: 0.4rem; font-size: 0.8rem; color: #fff; }
.badge.ok { background: #2a8; }
.badge.error { background: #c44; }
.badge.running { background: #c83; }
.badge.stopped { background: #777; }
.muted { color: #888; }
"#;

#[component]
fn Page(title: String, children: Children) -> impl IntoView {
    view! {
        <html lang="en">
            <head>
                <meta charset="utf-8"/>
                <meta name="viewport" content="width=device-width, initial-scale=1"/>
                <title>{format!("{title} · FediFetcher")}</title>
                <style>{CSS}</style>
            </head>
            <body>
                <header>
                    <h1>
                        <a href="/">"FediFetcher"</a>
                        <span class="version">"v"{VERSION}</span>
                    </h1>
                    <nav>
                        <a href="/">"Dashboard"</a>
                        <a href="/hosts">"Hosts"</a>
                        <a href="/logs">"Logs"</a>
                        <a href="/config">"Config"</a>
                        <a href="/debug">"Debug"</a>
                    </nav>
                </header>
                <main>{children()}</main>
            </body>
        </html>
    }
}

#[component]
fn Dashboard(state: Arc<AppState>, history: Vec<RunRow>) -> impl IntoView {
    let config = state.config.lock().unwrap().clone();
    let data = StateStore::new(config.clone()).load();
    let (status_label, status_class, is_running) = {
        let ui = state.ui.lock().unwrap();
        match &ui.status {
            RunStatus::Idle => ("Idle".to_string(), "ok".to_string(), false),
            RunStatus::Running { since } => (
                format!("Running since {}", timeutil::fmt_state_ts(*since)),
                "running".to_string(),
                true,
            ),
        }
    };
    let tasks = config.enabled_tasks();
    let stats = vec![
        ("seen urls".to_string(), data.seen_urls.len()),
        ("known followings".to_string(), data.known_followings.len()),
        (
            "recently checked users".to_string(),
            data.recently_checked_users.len(),
        ),
        ("seen hosts".to_string(), data.seen_hosts.len()),
        (
            "recently checked contexts".to_string(),
            data.recently_checked_context.len(),
        ),
        (
            "replied-toot lookups".to_string(),
            data.replied_toot_server_ids.len(),
        ),
    ];

    view! {
        <Page title="Dashboard".to_string()>
            <section>
                <h2>"Run"</h2>
                <p>
                    "Status: "
                    <span class=format!("badge {status_class}")>{status_label}</span>
                </p>
                {if is_running {
                    view! {
                        <form class="run" method="post" action="/stop">
                            <button type="submit">"Stop the running FediFetcher"</button>
                        </form>
                    }.into_any()
                } else {
                    view! {
                        <form class="run" method="post" action="/run">
                            <button type="submit">"Run FediFetcher now"</button>
                        </form>
                    }.into_any()
                }}
                <p class="muted">
                    "Server " <code>{config.server.clone()}</code>
                    " · " {config.access_tokens.len()} " access token(s)"
                </p>
            </section>
            <section>
                <h2>"Enabled tasks"</h2>
                {if tasks.is_empty() {
                    view! { <p class="muted">"None — nothing is enabled."</p> }.into_any()
                } else {
                    view! {
                        <ul>
                            {tasks.into_iter().map(|task| view! { <li>{task}</li> }).collect::<Vec<_>>()}
                        </ul>
                    }.into_any()
                }}
            </section>
            <section>
                <h2>"State"</h2>
                <table>
                    <tbody>
                        {stats
                            .into_iter()
                            .map(|(name, count)| {
                                view! {
                                    <tr>
                                        <td>{name}</td>
                                        <td>{count}</td>
                                    </tr>
                                }
                            })
                            .collect::<Vec<_>>()}
                    </tbody>
                </table>
            </section>
            <section>
                <h2>"Recent runs"</h2>
                {if history.is_empty() {
                    view! { <p class="muted">"No runs yet."</p> }.into_any()
                } else {
                    view! {
                        <table>
                            <thead>
                                <tr>
                                    <th>"Started"</th>
                                    <th>"Finished"</th>
                                    <th>"Outcome"</th>
                                    <th>"Message"</th>
                                </tr>
                            </thead>
                            <tbody>
                                {history
                                    .into_iter()
                                    .map(|record| {
                                        let badge = match record.outcome.as_str() {
                                            "ok" => "ok",
                                            "stopped" => "stopped",
                                            _ => "error",
                                        };
                                        view! {
                                            <tr>
                                                <td>{record.started}</td>
                                                <td>{record.finished}</td>
                                                <td><span class=format!("badge {badge}")>{record.outcome}</span></td>
                                                <td>{record.message}</td>
                                            </tr>
                                        }
                                    })
                                    .collect::<Vec<_>>()}
                            </tbody>
                        </table>
                    }.into_any()
                }}
            </section>
        </Page>
    }
}

#[component]
fn HostsPage(state: Arc<AppState>, sort: HostSort, order: LogOrder) -> impl IntoView {
    let data = StateStore::new(state.config.lock().unwrap().clone()).load();
    let mut rows: Vec<(String, String, String, String, String)> = Vec::new();
    for (host, info) in data.seen_hosts.iter() {
        let text = |key: &str| {
            info.get(key)
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string()
        };
        let apis = [
            "mastodonApiSupport",
            "misskeyApiSupport",
            "lemmyApiSupport",
            "peertubeApiSupport",
        ]
        .iter()
        .filter(|key| info.get(**key).and_then(Value::as_bool) == Some(true))
        .map(|key| key.trim_end_matches("ApiSupport"))
        .collect::<Vec<_>>()
        .join(", ");
        let (software, apis) = if info.get("info") == Some(&Value::Null) {
            match info.get("error").and_then(Value::as_str) {
                Some(reason) if !reason.is_empty() => {
                    (format!("lookup failed: {reason}"), String::new())
                }
                _ => ("lookup failed".to_string(), String::new()),
            }
        } else {
            (text("software"), if apis.is_empty() { "none".to_string() } else { apis })
        };
        rows.push((host.clone(), software, text("version"), apis, text("last_checked")));
    }
    // hosts alphabetically; the tie order for every other column
    rows.sort();
    // a stable sort, so equal keys keep the alphabetical order above
    rows.sort_by(|a, b| match sort {
        HostSort::Host => a.0.cmp(&b.0),
        HostSort::Software => a.1.cmp(&b.1),
        HostSort::Version => a.2.cmp(&b.2),
        HostSort::Apis => a.3.cmp(&b.3),
        HostSort::LastChecked => a.4.cmp(&b.4),
    });
    if order == LogOrder::Desc {
        rows.reverse();
    }

    view! {
        <Page title="Hosts".to_string()>
            <section>
                <h2>"Seen hosts"</h2>
                <p class="muted">
                    "Every server FediFetcher has contacted, and what it found. Click a column heading to sort by it; click it again to flip the direction."
                </p>
                {if rows.is_empty() {
                    view! { <p class="muted">"No hosts seen yet."</p> }.into_any()
                } else {
                    view! {
                        <table>
                            <thead>
                                <tr>
                                    <th>{sort_link("Host", HostSort::Host.param(), sort.param(), order, "/hosts", LogOrder::Asc)}</th>
                                    <th>{sort_link("Software", HostSort::Software.param(), sort.param(), order, "/hosts", LogOrder::Asc)}</th>
                                    <th>{sort_link("Version", HostSort::Version.param(), sort.param(), order, "/hosts", LogOrder::Asc)}</th>
                                    <th>{sort_link("APIs", HostSort::Apis.param(), sort.param(), order, "/hosts", LogOrder::Asc)}</th>
                                    <th>{sort_link("Last checked", HostSort::LastChecked.param(), sort.param(), order, "/hosts", LogOrder::Asc)}</th>
                                </tr>
                            </thead>
                            <tbody>
                                {rows
                                    .into_iter()
                                    .map(|(host, software, version, apis, last_checked)| {
                                        view! {
                                            <tr>
                                                <td><code>{host}</code></td>
                                                <td>{software}</td>
                                                <td>{version}</td>
                                                <td>{apis}</td>
                                                <td>{last_checked}</td>
                                            </tr>
                                        }
                                    })
                                    .collect::<Vec<_>>()}
                            </tbody>
                        </table>
                    }.into_any()
                }}
            </section>
        </Page>
    }
}

#[component]
fn LogsPage(rows: Vec<LogRow>, sort: LogSort, order: LogOrder) -> impl IntoView {
    view! {
        <Page title="Logs".to_string()>
            <section>
                <h2>"Recent log"</h2>
                <p class="muted">
                    "The last " {LOG_LINES_SHOWN} " entries from the database in the state directory, newest first. Click a column heading to sort by it; click it again to flip the direction."
                </p>
                <form method="post" action="/logs/clear">
                    <button type="submit">"Clear log"</button>
                </form>
                {if rows.is_empty() {
                    view! { <p class="muted">"No log entries yet."</p> }.into_any()
                } else {
                    view! {
                        <table>
                            <thead>
                                <tr>
                                    <th>{sort_link("Time", LogSort::Time.param(), sort.param(), order, "/logs", LogOrder::Desc)}</th>
                                    <th>{sort_link("Level", LogSort::Level.param(), sort.param(), order, "/logs", LogOrder::Desc)}</th>
                                    <th>{sort_link("Message", LogSort::Message.param(), sort.param(), order, "/logs", LogOrder::Desc)}</th>
                                </tr>
                            </thead>
                            <tbody>
                                {rows
                                    .into_iter()
                                    .map(|row| {
                                        view! {
                                            <tr>
                                                <td>{row.ts}</td>
                                                <td>{row.level}</td>
                                                <td>{row.message}</td>
                                            </tr>
                                        }
                                    })
                                    .collect::<Vec<_>>()}
                            </tbody>
                        </table>
                    }.into_any()
                }}
            </section>
        </Page>
    }
}

/// A column heading that links to the same page sorted by that column.
/// Clicking the column that is already sorted flips the direction; clicking
/// a new column sorts it the page's default way (`fresh`).
fn sort_link(
    label: &str,
    column: &str,
    current: &str,
    order: LogOrder,
    page: &str,
    fresh: LogOrder,
) -> AnyView {
    let dir = if current == column {
        match order {
            LogOrder::Desc => "asc",
            LogOrder::Asc => "desc",
        }
    } else {
        match fresh {
            LogOrder::Desc => "desc",
            LogOrder::Asc => "asc",
        }
    };
    let href = format!("{page}?sort={column}&dir={dir}");
    let label = label.to_string();
    let shown = if current == column {
        match order {
            LogOrder::Desc => " (desc)",
            LogOrder::Asc => " (asc)",
        }
    } else {
        ""
    };
    view! { <a href={href}>{label}{shown}</a> }.into_any()
}

#[component]
fn ConfigPage(state: Arc<AppState>) -> impl IntoView {
    let config = state.config.lock().unwrap().clone();
    let config_file = state.config_file.display().to_string();
    let optional = |value: &Option<String>| value.clone().unwrap_or_default();
    let mut rows: Vec<AnyView> = Vec::new();
    rows.push(text_row("server", &config.server));
    rows.push(tokens_row(&config));
    rows.push(text_row("user", &config.user));
    rows.push(int_row("reply_interval_in_hours", config.reply_interval_in_hours));
    rows.push(int_row("home_timeline_length", config.home_timeline_length));
    rows.push(int_row("max_followings", config.max_followings));
    rows.push(int_row("max_followers", config.max_followers));
    rows.push(int_row("max_follow_requests", config.max_follow_requests));
    rows.push(int_row("max_bookmarks", config.max_bookmarks));
    rows.push(int_row("max_favourites", config.max_favourites));
    rows.push(int_row("from_notifications", config.from_notifications));
    rows.push(bool_row("from_lists", config.from_lists));
    rows.push(int_row("max_list_length", config.max_list_length));
    rows.push(int_row("max_list_accounts", config.max_list_accounts));
    rows.push(int_row("max_posts_per_account", config.max_posts_per_account));
    rows.push(bool_row("backfill_with_context", config.backfill_with_context));
    rows.push(bool_row("backfill_mentioned_users", config.backfill_mentioned_users));
    rows.push(bool_row("respect_robots_txt", config.respect_robots_txt));
    rows.push(bool_row("respect_user_opt_out", config.respect_user_opt_out));
    rows.push(int_row("remember_users_for_hours", config.remember_users_for_hours));
    rows.push(int_row("remember_hosts_for_days", config.remember_hosts_for_days));
    rows.push(int_row("http_timeout", config.http_timeout));
    rows.push(bool_row("hide_server_in_user_agent", config.hide_server_in_user_agent));
    rows.push(text_row("instance_blocklist", &config.instance_blocklist.join(", ")));
    rows.push(text_row("state_dir", &config.state_dir.display().to_string()));
    rows.push(text_row(
        "lock_file",
        &config.lock_file.as_ref().map(|path| path.display().to_string()).unwrap_or_default(),
    ));
    rows.push(int_row("lock_hours", config.lock_hours));
    rows.push(text_row("on_start", &optional(&config.on_start)));
    rows.push(text_row("on_done", &optional(&config.on_done)));
    rows.push(text_row("on_fail", &optional(&config.on_fail)));
    rows.push(log_level_row(&config.log_level));
    rows.push(text_row("log_format", &config.log_format));
    view! {
        <Page title="Config".to_string()>
            <section>
                <h2>"Edit configuration"</h2>
                <p class="muted">
                    "Saved to " <code>{config_file}</code>
                    ". Runs use the saved values; log level and format apply on restart."
                </p>
                <p class="muted">"Hover a field for what it does."</p>
                <form method="post" action="/config">
                    <table>
                        <tbody>{rows}</tbody>
                    </table>
                    <p><button type="submit">"Save configuration"</button></p>
                </form>
            </section>
        </Page>
    }
}

#[component]
fn DebugPage(state: Arc<AppState>, result: Option<DebugOutcome>) -> impl IntoView {
    let config = state.config.lock().unwrap().clone();
    let server = if config.server.is_empty() {
        "not set".to_string()
    } else {
        config.server.clone()
    };
    let token_hint = config
        .access_tokens
        .first()
        .map(|token| masked(token))
        .unwrap_or_else(|| "none".to_string());
    view! {
        <Page title="Debug".to_string()>
            <section>
                <h2>"Test connection"</h2>
                <p class="muted">
                    "Asks " <code>{server.clone()}</code> " what it is, and whether it accepts the first access token (" {token_hint.clone()} ")."
                </p>
                <form method="post" action="/debug">
                    <input type="hidden" name="action" value="connect"/>
                    <button type="submit">"Test connection"</button>
                </form>
            </section>
            <section>
                <h2>"Look up a user"</h2>
                <form method="post" action="/debug">
                    <input type="hidden" name="action" value="user"/>
                    <p>
                        <input type="text" name="user" placeholder="user@example.social" required/>
                        <button type="submit">"Look up"</button>
                    </p>
                </form>
            </section>
            <section>
                <h2>"Recent posts"</h2>
                <form method="post" action="/debug">
                    <input type="hidden" name="action" value="posts"/>
                    <p>
                        <input type="text" name="user" placeholder="user@example.social" required/>
                        <input type="number" name="limit" value="10" min="1" max="40"/>
                        " posts"
                        <button type="submit">"Fetch posts"</button>
                    </p>
                </form>
            </section>
            {result.map(|outcome| debug_result_view(&outcome))}
        </Page>
    }
}

/// The result of a debug action, as a section at the bottom of the page.
fn debug_result_view(outcome: &DebugOutcome) -> AnyView {
    match outcome {
        DebugOutcome::Connected { instance, account } => {
            let instance_rows = [
                ("title", text_field(instance, "title")),
                ("uri", text_field(instance, "uri")),
                ("version", text_field(instance, "version")),
            ];
            view! {
                <section class="result">
                    <h2>"Connection"</h2>
                    <p>
                        <span class="badge ok">"ok"</span>
                        " The server answered, and accepted the access token."
                    </p>
                    <table>
                        <tbody>
                            {instance_rows
                                .iter()
                                .map(|(name, value)| {
                                    view! {
                                        <tr>
                                            <td>{name.to_string()}</td>
                                            <td>{value.clone()}</td>
                                        </tr>
                                    }
                                })
                                .collect::<Vec<_>>()}
                        </tbody>
                    </table>
                    {account_view(account)}
                </section>
            }
            .into_any()
        }
        DebugOutcome::User { acct, account } => view! {
            <section class="result">
                <h2>"User"</h2>
                <p>
                    <span class="badge ok">"ok"</span>
                    " Found " <code>{acct.clone()}</code> "."
                </p>
                {account_view(account)}
            </section>
        }
        .into_any(),
        DebugOutcome::Posts {
            acct,
            account,
            posts,
        } => {
            let rows: Vec<AnyView> = posts
                .iter()
                .map(|post| {
                    let url = text_field(post, "url");
                    let content = snippet(post.get("content").and_then(Value::as_str).unwrap_or(""));
                    view! {
                        <tr>
                            <td>{text_field(post, "created_at")}</td>
                            <td>{text_field(post, "visibility")}</td>
                            <td>{int_field(post, "replies_count")}</td>
                            <td>{int_field(post, "reblogs_count")}</td>
                            <td>{int_field(post, "favourites_count")}</td>
                            <td><a href={url.clone()}>{content}</a></td>
                        </tr>
                    }
                    .into_any()
                })
                .collect();
            view! {
                <section class="result">
                    <h2>"Recent posts"</h2>
                    <p>
                        <span class="badge ok">"ok"</span>
                        " " {posts.len().to_string()} " post(s) by "
                        <code>{acct.clone()}</code>
                        " (" <code>{text_field(account, "acct")}</code> ")."
                    </p>
                    {if rows.is_empty() {
                        view! { <p class="muted">"No posts."</p> }.into_any()
                    } else {
                        view! {
                            <table>
                                <thead>
                                    <tr>
                                        <th>"Created"</th>
                                        <th>"Visibility"</th>
                                        <th>"Replies"</th>
                                        <th>"Boosts"</th>
                                        <th>"Favourites"</th>
                                        <th>"Post"</th>
                                    </tr>
                                </thead>
                                <tbody>{rows}</tbody>
                            </table>
                        }
                        .into_any()
                    }}
                </section>
            }
            .into_any()
        }
        DebugOutcome::Failed { message } => view! {
            <section class="result">
                <h2>"Result"</h2>
                <p><span class="badge error">"failed"</span></p>
                <pre>{message.clone()}</pre>
            </section>
        }
        .into_any(),
    }
}

/// An account's basic fields, as a table.
fn account_view(account: &Value) -> AnyView {
    let url = text_field(account, "url");
    let mut rows: Vec<AnyView> = [
        ("acct", text_field(account, "acct")),
        ("display name", text_field(account, "display_name")),
        ("id", text_field(account, "id")),
        ("created", text_field(account, "created_at")),
        ("followers", int_field(account, "followers_count")),
        ("following", int_field(account, "following_count")),
        ("posts", int_field(account, "statuses_count")),
        ("bot", text_field(account, "bot")),
        ("locked", text_field(account, "locked")),
    ]
    .iter()
    .map(|(name, value)| {
        view! {
            <tr>
                <td>{name.to_string()}</td>
                <td>{value.clone()}</td>
            </tr>
        }
        .into_any()
    })
    .collect();
    let link = url.clone();
    rows.push(
        view! {
            <tr>
                <td>"url"</td>
                <td><a href={link}>{url}</a></td>
            </tr>
        }
        .into_any(),
    );
    let note = snippet(account.get("note").and_then(Value::as_str).unwrap_or(""));
    if !note.is_empty() {
        rows.push(
            view! {
                <tr>
                    <td>"note"</td>
                    <td>{note}</td>
                </tr>
            }
            .into_any(),
        );
    }
    view! {
        <table>
            <tbody>{rows}</tbody>
        </table>
    }
    .into_any()
}

/// A string field of a JSON object, or "-" when it is absent or empty.
fn text_field(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
        .unwrap_or("-")
        .to_string()
}

/// A count field of a JSON object, or "-" when it is absent.
fn int_field(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_i64)
        .map(|count| count.to_string())
        .unwrap_or_else(|| "-".to_string())
}

/// Plain text for a bit of HTML: tags gone, entities read, one line, short.
fn snippet(html: &str) -> String {
    let without_tags = Regex::new(r"<[^>]*>").unwrap().replace_all(html, " ");
    let text = without_tags
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'");
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() > 200 {
        let mut truncated: String = collapsed.chars().take(200).collect();
        truncated.push_str("...");
        truncated
    } else {
        collapsed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn snippet_strips_html_and_reads_entities() {
        assert_eq!(snippet("<p>Hello <b>world</b></p>"), "Hello world");
        assert_eq!(snippet("a &amp; b &lt; c &gt; d"), "a & b < c > d");
        assert_eq!(snippet("<p>one\n  two</p>"), "one two");
        assert_eq!(snippet(""), "");
    }

    #[test]
    fn snippet_truncates_long_text() {
        let long = format!("<p>{}</p>", "x".repeat(500));
        let text = snippet(&long);
        assert_eq!(text.len(), 203, "200 characters and an ellipsis");
        assert!(text.ends_with("..."));
    }

    #[test]
    fn json_fields_render_or_dash() {
        let account = json!({"acct": "me", "followers_count": 12, "note": ""});
        assert_eq!(text_field(&account, "acct"), "me");
        assert_eq!(text_field(&account, "url"), "-");
        assert_eq!(text_field(&account, "note"), "-", "empty is absent");
        assert_eq!(int_field(&account, "followers_count"), "12");
        assert_eq!(int_field(&account, "id"), "-");
    }
}
