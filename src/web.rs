//! A server-rendered web UI: dashboard, run trigger, hosts, logs and the
//! effective config. No JavaScript anywhere — every page is HTML from leptos
//! SSR, and the only interactivity is a form POST that redirects back.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use axum::extract::State as AxumState;
use axum::response::{Html, Redirect};
use axum::routing::{get, post};
use axum::Router;
use leptos::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use time::OffsetDateTime;
use tokio::sync::Mutex as AsyncMutex;

use crate::api::mastodon::masked;
use crate::config::Config;
use crate::logging;
use crate::run::run_once;
use crate::store::StateStore;
use crate::timeutil;
use crate::VERSION;

/// The most runs remembered for the dashboard's history.
const HISTORY_CAPACITY: usize = 50;

/// The most log lines shown on /logs.
const LOG_LINES_SHOWN: usize = 200;

struct AppState {
    config: Config,
    ui: Mutex<UiState>,
    /// Held for the duration of a web-triggered run, so two clicks cannot
    /// overlap; the lock file keeps other processes out.
    run_guard: Arc<AsyncMutex<()>>,
}

struct UiState {
    status: RunStatus,
    history: VecDeque<RunRecord>,
}

enum RunStatus {
    Idle,
    Running { since: OffsetDateTime },
}

#[derive(Clone, Serialize, Deserialize)]
struct RunRecord {
    started: OffsetDateTime,
    finished: OffsetDateTime,
    outcome: String,
    message: String,
}

/// Serve the web UI until the process is stopped.
pub async fn serve(config: Config, bind: &str, port: u16) -> std::io::Result<()> {
    let app = app(config);

    let addr: std::net::SocketAddr = format!("{bind}:{port}")
        .parse()
        .map_err(|ex| std::io::Error::new(std::io::ErrorKind::InvalidInput, ex))?;
    tracing::info!("FediFetcher web UI listening on http://{addr}");
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await
}

/// Build the web UI's router: the dashboard, the run trigger, hosts, logs,
/// config, and the health check.
pub fn app(config: Config) -> Router {
    let mut history = VecDeque::new();
    if let Some(record) = load_last_run(&config) {
        history.push_back(record);
    }
    let state = Arc::new(AppState {
        config,
        ui: Mutex::new(UiState {
            status: RunStatus::Idle,
            history,
        }),
        run_guard: Arc::new(AsyncMutex::new(())),
    });

    Router::new()
        .route("/", get(dashboard))
        .route("/run", post(run))
        .route("/hosts", get(hosts))
        .route("/logs", get(logs))
        .route("/config", get(config_page))
        .route("/healthz", get(healthz))
        .with_state(state)
}

async fn dashboard(AxumState(state): AxumState<Arc<AppState>>) -> Html<String> {
    render(move || view! { <Dashboard state=state /> })
}

async fn hosts(AxumState(state): AxumState<Arc<AppState>>) -> Html<String> {
    render(move || view! { <HostsPage state=state /> })
}

async fn logs() -> Html<String> {
    render(|| view! { <LogsPage /> })
}

async fn config_page(AxumState(state): AxumState<Arc<AppState>>) -> Html<String> {
    render(move || view! { <ConfigPage state=state /> })
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
        tokio::spawn(async move {
            let started = timeutil::now();
            {
                let mut ui = state.ui.lock().unwrap();
                ui.status = RunStatus::Running { since: started };
            }

            let outcome = run_once(state.config.clone()).await;
            let finished = timeutil::now();
            let (outcome_label, message) = match &outcome {
                Ok(message) => ("ok".to_string(), message.clone()),
                Err(ex) => ("error".to_string(), ex.to_string()),
            };
            let record = RunRecord {
                started,
                finished,
                outcome: outcome_label,
                message,
            };
            persist_last_run(&state.config, &record);

            let mut ui = state.ui.lock().unwrap();
            ui.status = RunStatus::Idle;
            ui.history.push_back(record);
            if ui.history.len() > HISTORY_CAPACITY {
                ui.history.pop_front();
            }
            drop(ui);
            drop(guard);
        });
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

/// The last run the web UI triggered, remembered across restarts.
fn last_run_file(config: &Config) -> std::path::PathBuf {
    config.state_dir.join("web_ui_last_run.json")
}

fn persist_last_run(config: &Config, record: &RunRecord) {
    std::fs::create_dir_all(&config.state_dir).ok();
    if let Ok(json) = serde_json::to_string_pretty(record) {
        std::fs::write(last_run_file(config), json).ok();
    }
}

fn load_last_run(config: &Config) -> Option<RunRecord> {
    let text = std::fs::read_to_string(last_run_file(config)).ok()?;
    serde_json::from_str(&text).ok()
}

/// Which tasks a run will do, in the order it will do them.
fn enabled_tasks(config: &Config) -> Vec<&'static str> {
    let mut tasks = Vec::new();
    if config.from_lists {
        tasks.push("lists");
    }
    if config.reply_interval_in_hours > 0 {
        tasks.push("reply context");
    }
    if config.home_timeline_length > 0 {
        tasks.push("home timeline");
    }
    if config.max_followings > 0 {
        tasks.push("followings");
    }
    if config.max_followers > 0 {
        tasks.push("followers");
    }
    if config.max_follow_requests > 0 {
        tasks.push("follow requests");
    }
    if config.from_notifications > 0 {
        tasks.push("notifications");
    }
    if config.max_bookmarks > 0 {
        tasks.push("bookmarks");
    }
    if config.max_favourites > 0 {
        tasks.push("favourites");
    }
    tasks
}

/// The effective config as rows of (name, value), tokens masked.
fn config_rows(config: &Config) -> Vec<(String, String)> {
    let yes_no = |value: bool| if value { "yes" } else { "no" }.to_string();
    vec![
        ("server".to_string(), config.server.clone()),
        (
            "access_tokens".to_string(),
            config
                .access_tokens
                .iter()
                .map(|token| masked(token))
                .collect::<Vec<_>>()
                .join(", "),
        ),
        (
            "user".to_string(),
            if config.user.is_empty() {
                "-".to_string()
            } else {
                config.user.clone()
            },
        ),
        (
            "reply_interval_in_hours".to_string(),
            config.reply_interval_in_hours.to_string(),
        ),
        (
            "home_timeline_length".to_string(),
            config.home_timeline_length.to_string(),
        ),
        (
            "max_followings".to_string(),
            config.max_followings.to_string(),
        ),
        ("max_followers".to_string(), config.max_followers.to_string()),
        (
            "max_follow_requests".to_string(),
            config.max_follow_requests.to_string(),
        ),
        ("max_bookmarks".to_string(), config.max_bookmarks.to_string()),
        (
            "max_favourites".to_string(),
            config.max_favourites.to_string(),
        ),
        (
            "from_notifications".to_string(),
            config.from_notifications.to_string(),
        ),
        ("from_lists".to_string(), yes_no(config.from_lists)),
        (
            "max_list_length".to_string(),
            config.max_list_length.to_string(),
        ),
        (
            "max_list_accounts".to_string(),
            config.max_list_accounts.to_string(),
        ),
        (
            "max_posts_per_account".to_string(),
            config.max_posts_per_account.to_string(),
        ),
        (
            "backfill_with_context".to_string(),
            yes_no(config.backfill_with_context),
        ),
        (
            "backfill_mentioned_users".to_string(),
            yes_no(config.backfill_mentioned_users),
        ),
        (
            "respect_robots_txt".to_string(),
            yes_no(config.respect_robots_txt),
        ),
        (
            "respect_user_opt_out".to_string(),
            yes_no(config.respect_user_opt_out),
        ),
        (
            "remember_users_for_hours".to_string(),
            config.remember_users_for_hours.to_string(),
        ),
        (
            "remember_hosts_for_days".to_string(),
            config.remember_hosts_for_days.to_string(),
        ),
        ("http_timeout".to_string(), config.http_timeout.to_string()),
        (
            "instance_blocklist".to_string(),
            config.instance_blocklist.join(", "),
        ),
        (
            "state_dir".to_string(),
            config.state_dir.display().to_string(),
        ),
        (
            "lock_file".to_string(),
            config
                .lock_file
                .as_ref()
                .map(|path| path.display().to_string())
                .unwrap_or_else(|| "-".to_string()),
        ),
        ("lock_hours".to_string(), config.lock_hours.to_string()),
        (
            "on_start".to_string(),
            config.on_start.clone().unwrap_or_else(|| "-".to_string()),
        ),
        (
            "on_done".to_string(),
            config.on_done.clone().unwrap_or_else(|| "-".to_string()),
        ),
        (
            "on_fail".to_string(),
            config.on_fail.clone().unwrap_or_else(|| "-".to_string()),
        ),
        ("log_level".to_string(), config.log_level.clone()),
        ("log_format".to_string(), config.log_format.clone()),
    ]
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
.badge { padding: 0.1rem 0.5rem; border-radius: 0.4rem; font-size: 0.8rem; color: #fff; }
.badge.ok { background: #2a8; }
.badge.error { background: #c44; }
.badge.running { background: #c83; }
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
                    </nav>
                </header>
                <main>{children()}</main>
            </body>
        </html>
    }
}

#[component]
fn Dashboard(state: Arc<AppState>) -> impl IntoView {
    let config = state.config.clone();
    let data = StateStore::new(config.clone()).load();
    let (status_label, status_class) = {
        let ui = state.ui.lock().unwrap();
        match &ui.status {
            RunStatus::Idle => ("Idle".to_string(), "ok".to_string()),
            RunStatus::Running { since } => (
                format!("Running since {}", timeutil::fmt_state_ts(*since)),
                "running".to_string(),
            ),
        }
    };
    let history: Vec<RunRecord> = state
        .ui
        .lock()
        .unwrap()
        .history
        .iter()
        .rev()
        .take(HISTORY_CAPACITY)
        .cloned()
        .collect();
    let tasks = enabled_tasks(&config);
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
                <form class="run" method="post" action="/run">
                    <button type="submit">"Run FediFetcher now"</button>
                </form>
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
                                        let badge = if record.outcome == "ok" { "ok" } else { "error" };
                                        view! {
                                            <tr>
                                                <td>{timeutil::fmt_state_ts(record.started)}</td>
                                                <td>{timeutil::fmt_state_ts(record.finished)}</td>
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
fn HostsPage(state: Arc<AppState>) -> impl IntoView {
    let data = StateStore::new(state.config.clone()).load();
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
            ("lookup failed".to_string(), String::new())
        } else {
            (text("software"), if apis.is_empty() { "none".to_string() } else { apis })
        };
        rows.push((host.clone(), software, text("version"), apis, text("last_checked")));
    }
    rows.sort();

    view! {
        <Page title="Hosts".to_string()>
            <section>
                <h2>"Seen hosts"</h2>
                {if rows.is_empty() {
                    view! { <p class="muted">"No hosts seen yet."</p> }.into_any()
                } else {
                    view! {
                        <table>
                            <thead>
                                <tr>
                                    <th>"Host"</th>
                                    <th>"Software"</th>
                                    <th>"Version"</th>
                                    <th>"APIs"</th>
                                    <th>"Last checked"</th>
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
fn LogsPage() -> impl IntoView {
    let lines = logging::recent_log_lines(LOG_LINES_SHOWN);
    view! {
        <Page title="Logs".to_string()>
            <section>
                <h2>"Recent log"</h2>
                <p class="muted">"The last " {LOG_LINES_SHOWN} " lines."</p>
                <pre>{lines.join("\n")}</pre>
            </section>
        </Page>
    }
}

#[component]
fn ConfigPage(state: Arc<AppState>) -> impl IntoView {
    let rows = config_rows(&state.config);
    view! {
        <Page title="Config".to_string()>
            <section>
                <h2>"Effective configuration"</h2>
                <table>
                    <tbody>
                        {rows
                            .into_iter()
                            .map(|(name, value)| {
                                view! {
                                    <tr>
                                        <td><code>{name}</code></td>
                                        <td>{value}</td>
                                    </tr>
                                }
                            })
                            .collect::<Vec<_>>()}
                    </tbody>
                </table>
            </section>
        </Page>
    }
}
