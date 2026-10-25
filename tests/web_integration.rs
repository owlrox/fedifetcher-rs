//! The web UI over real HTTP: every page renders, the run trigger answers
//! with a redirect, and there is no JavaScript anywhere.

mod common;

use fedifetcher::api::mastodon::masked;
use fedifetcher::web;
use httpmock::Method;
use httpmock::prelude::*;

const TOKEN: &str = "smoke-secret-token-123";
const OAUTH_TOKEN: &str = "the-oauth-token";

/// Start the web UI on an ephemeral port, returning its base URL.
async fn spawn_ui(dir: &std::path::Path) -> String {
    let mut config = common::test_config(dir, "example.social");
    config.access_tokens = vec![TOKEN.to_string()];
    spawn_ui_with(config).await.0
}

/// Start the web UI with the given config: its base URL, and the database
/// the UI reads and writes.
async fn spawn_ui_with(config: fedifetcher::config::Config) -> (String, sqlx::SqlitePool) {
    let db = fedifetcher::db::open(&config).await.unwrap();
    let app = web::app(config, db.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });
    (format!("http://{addr}"), db)
}

/// A client that does not follow redirects, so 303s can be seen.
fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap()
}

async fn get(url: &str) -> (u16, String) {
    let resp = client().get(url).send().await.unwrap();
    let status = resp.status().as_u16();
    let body = resp.text().await.unwrap();
    (status, body)
}

/// GET a page with a cookie header: (status, body).
async fn get_with_cookie(url: &str, cookie: &str) -> (u16, String) {
    let resp = client()
        .get(url)
        .header("Cookie", cookie)
        .send()
        .await
        .unwrap();
    let status = resp.status().as_u16();
    let body = resp.text().await.unwrap();
    (status, body)
}

/// Poll the page at `path` until it contains any of the `markers`, polling
/// as the timing conventions require, and return the matching page.
async fn page_containing(base: &str, path: &str, markers: &[&str]) -> String {
    for _ in 0..100 {
        let (_, page) = get(&format!("{base}{path}")).await;
        if markers.iter().any(|marker| page.contains(marker)) {
            return page;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    panic!("the page {path} never contained any of {markers:?}");
}

/// POST a urlencoded form to /config: (status, Location, body).
async fn post_config(base: &str, form: &str) -> (u16, Option<String>, String) {
    let resp = client()
        .post(format!("{base}/config"))
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(form.to_string())
        .send()
        .await
        .unwrap();
    let status = resp.status().as_u16();
    let location = resp
        .headers()
        .get("Location")
        .map(|value| value.to_str().unwrap().to_string());
    let body = resp.text().await.unwrap();
    (status, location, body)
}

/// POST a urlencoded form to /debug: (status, body).
async fn post_debug(base: &str, form: &str) -> (u16, String) {
    let resp = client()
        .post(format!("{base}/debug"))
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(form.to_string())
        .send()
        .await
        .unwrap();
    let status = resp.status().as_u16();
    let body = resp.text().await.unwrap();
    (status, body)
}

/// Read the saved configuration back from the database into a Config.
async fn saved_config(pool: &sqlx::SqlitePool) -> fedifetcher::config::Config {
    let raw = fedifetcher::db::load_config(pool).await.unwrap();
    // lenient: a readback after a disconnect legitimately has no tokens
    fedifetcher::config::Config::from_values_lenient(raw).unwrap()
}

/// Every log line the process emits, for the page-load logging test. The
/// web UI's handlers run on the server's tasks, not the test's thread, so
/// unlike LogCapture this captures every thread. One test installs it,
/// once per test binary.
static PAGE_LOAD_LINES: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

/// Install the page-load capture; a second call is a no-op.
fn install_page_load_capture() {
    /// The `message` field of an event.
    #[derive(Default)]
    struct MessageVisitor {
        message: String,
    }

    impl tracing::field::Visit for MessageVisitor {
        fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
            if field.name() == "message" {
                self.message = format!("{value:?}");
            }
        }
    }

    struct Capture;

    impl tracing::Subscriber for Capture {
        fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
            true
        }

        fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
            tracing::span::Id::from_u64(1)
        }

        fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}

        fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}

        fn event(&self, event: &tracing::Event<'_>) {
            let mut visitor = MessageVisitor::default();
            event.record(&mut visitor);
            PAGE_LOAD_LINES.lock().unwrap().push(visitor.message);
        }

        fn enter(&self, _: &tracing::span::Id) {}

        fn exit(&self, _: &tracing::span::Id) {}

        fn register_callsite(
            &self,
            _: &'static tracing::Metadata<'static>,
        ) -> tracing::subscriber::Interest {
            tracing::subscriber::Interest::always()
        }

        fn max_level_hint(&self) -> Option<tracing::level_filters::LevelFilter> {
            Some(tracing::level_filters::LevelFilter::TRACE)
        }
    }

    let _ = tracing::subscriber::set_global_default(Capture);
}

/// The lines captured so far.
fn page_load_lines() -> Vec<String> {
    PAGE_LOAD_LINES.lock().unwrap().clone()
}

#[tokio::test]
async fn every_page_renders_without_javascript() {
    let dir = common::tmp_dir();
    let base = spawn_ui(&dir).await;

    for (path, marker) in [
        ("/", "Recent runs"),
        ("/data", "Seen hosts"),
        ("/logs", "Recent log"),
        ("/config", "access_tokens"),
        ("/debug", "Test connection"),
    ] {
        let (status, body) = get(&format!("{base}{path}")).await;
        assert_eq!(status, 200, "{path}");
        assert!(body.contains(marker), "{path} should contain {marker:?}");
        assert!(
            body.contains("FediFetcher-rs"),
            "{path} should name the app -rs"
        );
        assert!(
            !body.contains("<script"),
            "{path} must not contain JavaScript"
        );
    }

    let (status, body) = get(&format!("{base}/healthz")).await;
    assert_eq!(status, 200);
    assert_eq!(body, "ok");
}

#[tokio::test]
async fn the_config_page_has_tooltips_from_the_docs() {
    let dir = common::tmp_dir();
    let base = spawn_ui(&dir).await;

    let (status, body) = get(&format!("{base}/config")).await;
    assert_eq!(status, 200);
    // every field carries the original documentation as a hover tooltip
    assert!(
        body.contains("title=\"Required: the name of your server"),
        "the server field explains itself"
    );
    assert!(
        body.contains("backfill a specific user's followings/followers"),
        "the user field's tooltip answers what it does"
    );
    assert!(
        body.contains("title=\"Severity of events to log"),
        "the log level field has a tooltip"
    );
    assert!(
        body.contains("title=\"Required: one token per user whose timelines you fetch"),
        "the access tokens field has a tooltip"
    );
    // the tooltips carry the documented defaults and the scopes the
    // option's task spends, on their own lines
    assert!(
        body.contains("Default: 0 (off)"),
        "the task fields state their default: {body}"
    );
    assert!(
        body.contains("Scopes: read:lists (asked only while lists are enabled)"),
        "the lists field names its scope"
    );
    assert!(
        body.contains("Scopes: admin:read:accounts (asked only while this is enabled)"),
        "the reply field names its scope"
    );
    assert!(
        body.contains("name=\"hide_server_in_user_agent\""),
        "the user agent option is on the page"
    );
    assert!(
        body.contains("title=\"Omit your server's name from the User-Agent"),
        "the user agent option explains itself"
    );
    assert!(
        body.contains("name=\"log_page_loads\""),
        "the page-load logging option is on the page"
    );
    assert!(
        body.contains("title=\"Log a line for every page load"),
        "the page-load logging option explains itself"
    );
    assert!(
        body.contains("name=\"streaming\""),
        "the streaming option is on the page"
    );
    assert!(
        body.contains("title=\"Stream the home timeline over a long-lived connection"),
        "the streaming option explains itself"
    );
}

#[tokio::test]
async fn the_dashboard_shows_the_current_version() {
    let dir = common::tmp_dir();
    let base = spawn_ui(&dir).await;

    let (status, body) = get(&format!("{base}/")).await;
    assert_eq!(status, 200);
    // leptos separates the text nodes with comments, so match the version
    // itself rather than "v" glued to it
    assert!(
        body.contains(fedifetcher::VERSION),
        "the header shows the version being run"
    );
}

#[tokio::test]
async fn the_config_page_masks_the_access_token() {
    let dir = common::tmp_dir();
    let base = spawn_ui(&dir).await;

    let (status, body) = get(&format!("{base}/config")).await;
    assert_eq!(status, 200);
    assert!(body.contains(&masked(TOKEN)), "the token should be masked");
    assert!(!body.contains(TOKEN), "the token itself must not appear");
    assert!(
        body.contains("respect_robots_txt"),
        "the new options are shown"
    );
    assert!(
        body.contains("respect_user_opt_out"),
        "the new options are shown"
    );
}

#[tokio::test]
async fn posting_run_redirects_back_and_records_the_run() {
    let dir = common::tmp_dir();
    let base = spawn_ui(&dir).await;

    let resp = client().post(format!("{base}/run")).send().await.unwrap();
    assert_eq!(resp.status().as_u16(), 303, "the POST must redirect");
    assert_eq!(resp.headers().get("Location").unwrap(), "/");

    // the run it started finishes (nothing is enabled), and shows up in the
    // dashboard's history
    let body = page_containing(&base, "/", &["Processing finished in"]).await;
    assert!(
        body.contains("Processing finished in"),
        "the run should be recorded in the history"
    );
    assert!(body.contains("Recent runs"));

    // and the record is in the database, not just on the page
    let config = common::test_config(&dir, "example.social");
    let db = fedifetcher::db::open(&config).await.unwrap();
    let runs = fedifetcher::db::recent_runs(&db, 10).await.unwrap();
    assert_eq!(runs.len(), 1, "the run should be recorded in the database");
    assert_eq!(runs[0].outcome, "ok");
    assert!(runs[0].message.contains("Processing finished in"));
}

#[tokio::test]
async fn the_dashboard_shows_a_running_run_with_its_elapsed_time() {
    let dir = common::tmp_dir();
    let base = spawn_ui(&dir).await;

    // a run recorded as started but not finished, as one in progress is
    let config = common::test_config(&dir, "example.social");
    let db = fedifetcher::db::open(&config).await.unwrap();
    let started = fedifetcher::timeutil::fmt_state_ts(fedifetcher::timeutil::now());
    fedifetcher::db::start_run(&db, &started).await.unwrap();

    let (status, body) = get(&format!("{base}/")).await;
    assert_eq!(status, 200);
    assert!(
        body.contains(">running<"),
        "the running run is in the history"
    );
    assert!(
        body.contains(
            "<td> </td><td><span class=\"badge running\">running</span></td><td>running for"
        ),
        "the finished cell is blank and the message cell carries the elapsed time"
    );
    assert!(
        body.contains("badge running"),
        "the outcome gets its own badge"
    );
    assert!(
        body.contains("http-equiv=\"refresh\""),
        "the dashboard reloads itself while a run is live"
    );
}

/// The run status is derived from the run history — the database is the
/// one source — so the top bar's pill and the dashboard's status follow
/// the history's newest row, and a run starting or finishing between two
/// reads can no longer leave them disagreeing.
#[tokio::test]
async fn the_run_status_comes_from_the_run_history() {
    let dir = common::tmp_dir();
    let config = common::test_config(&dir, "example.social");
    let (base, db) = spawn_ui_with(config).await;

    // no runs yet: idle, in the top bar
    let (status, body) = get(&format!("{base}/")).await;
    assert_eq!(status, 200);
    assert!(
        body.contains("<span class=\"badge ok\">Idle</span>"),
        "idle, in the top bar: {body}"
    );

    // a run recorded as started but not finished, as one in progress is:
    // the pill follows the history row on every page, and the dashboard's
    // Run section follows it too
    let started = fedifetcher::timeutil::fmt_state_ts(fedifetcher::timeutil::now());
    let id = fedifetcher::db::start_run(&db, &started).await.unwrap();
    for page in ["/", "/logs"] {
        let (status, body) = get(&format!("{base}{page}")).await;
        assert_eq!(status, 200, "{page}");
        assert!(
            body.contains("<span class=\"badge running\">Running</span>"),
            "{page}: the pill follows the history row: {body}"
        );
        assert!(!body.contains(">Idle</span>"), "{page}: no stale idle pill");
    }

    // the row finishes: the pill goes back to idle, from the same read
    fedifetcher::db::finish_run(&db, id, &started, "ok", "done")
        .await
        .unwrap();
    let (status, body) = get(&format!("{base}/")).await;
    assert_eq!(status, 200);
    assert!(
        body.contains("<span class=\"badge ok\">Idle</span>"),
        "the pill is idle again: {body}"
    );
    assert!(!body.contains(">Running</span>"), "no stale running pill");
}

#[tokio::test]
async fn the_logs_page_renders_lines_from_the_database() {
    let dir = common::tmp_dir();
    let base = spawn_ui(&dir).await;

    // a line written straight to the database, as the log layer would
    let config = common::test_config(&dir, "example.social");
    let db = fedifetcher::db::open(&config).await.unwrap();
    fedifetcher::db::insert_log(
        &db,
        &fedifetcher::db::LogRow {
            ts: "2026-10-22 16:05:07 -05:00".to_string(),
            level: "INFO".to_string(),
            target: "fedifetcher::web".to_string(),
            message: "a line only the database knows".to_string(),
        },
    )
    .await
    .unwrap();

    let (status, body) = get(&format!("{base}/logs")).await;
    assert_eq!(status, 200);
    assert!(body.contains("<table"), "the log should render as a table");
    assert!(
        body.contains("<td>2026-10-22 16:05:07 -05:00</td>"),
        "the time column shows the entry's timestamp"
    );
    assert!(
        body.contains("<td><span class=\"badge info\">INFO</span></td>"),
        "the level column shows the entry's level as a pill"
    );
    assert!(
        body.contains("<td>a line only the database knows</td>"),
        "the message column shows the entry's message"
    );
    assert!(
        body.contains("action=\"/logs/clear\""),
        "the page offers a clear button"
    );
    assert!(
        body.contains("/logs?sort=level&amp;dir=desc"),
        "the column headings link to sorted views"
    );
}

/// Each level renders as a pill whose color class matches its severity.
#[tokio::test]
async fn the_logs_page_renders_levels_as_colored_pills() {
    let dir = common::tmp_dir();
    let base = spawn_ui(&dir).await;

    insert_log(&dir, "2026-10-22 16:05:01 -05:00", "DEBUG", "a debug line").await;
    insert_log(&dir, "2026-10-22 16:05:02 -05:00", "INFO", "an info line").await;
    insert_log(
        &dir,
        "2026-10-22 16:05:03 -05:00",
        "WARNING",
        "a warning line",
    )
    .await;
    insert_log(&dir, "2026-10-22 16:05:04 -05:00", "ERROR", "an error line").await;

    let (status, body) = get(&format!("{base}/logs")).await;
    assert_eq!(status, 200);
    for (level, class) in [
        ("DEBUG", "debug"),
        ("INFO", "info"),
        ("WARNING", "warning"),
        ("ERROR", "error"),
    ] {
        assert!(
            body.contains(&format!("<span class=\"badge {class}\">{level}</span>")),
            "the {level} line renders as a {class} pill"
        );
    }
}

/// Insert one log line straight into the database, as the log layer would.
async fn insert_log(dir: &std::path::Path, ts: &str, level: &str, message: &str) {
    let config = common::test_config(dir, "example.social");
    let db = fedifetcher::db::open(&config).await.unwrap();
    fedifetcher::db::insert_log(
        &db,
        &fedifetcher::db::LogRow {
            ts: ts.to_string(),
            level: level.to_string(),
            target: "fedifetcher::web".to_string(),
            message: message.to_string(),
        },
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn the_logs_page_shows_the_newest_entries_first() {
    let dir = common::tmp_dir();
    let base = spawn_ui(&dir).await;

    insert_log(&dir, "2026-10-22 16:05:01 -05:00", "INFO", "the older line").await;
    insert_log(&dir, "2026-10-22 16:05:09 -05:00", "INFO", "the newer line").await;

    let (status, body) = get(&format!("{base}/logs")).await;
    assert_eq!(status, 200);
    let newer = body.find("the newer line").expect("the newer line renders");
    let older = body.find("the older line").expect("the older line renders");
    assert!(newer < older, "the newest entry should come first");
}

#[tokio::test]
async fn the_logs_page_sorts_by_the_clicked_column() {
    let dir = common::tmp_dir();
    let base = spawn_ui(&dir).await;

    let ts = "2026-10-22 16:05:07 -05:00";
    insert_log(&dir, ts, "INFO", "an info line").await;
    insert_log(&dir, ts, "ERROR", "an error line").await;
    insert_log(&dir, ts, "DEBUG", "a debug line").await;

    // sorted by level, ascending: DEBUG, ERROR, INFO
    let (status, body) = get(&format!("{base}/logs?sort=level&dir=asc")).await;
    assert_eq!(status, 200);
    let debug = body.find("a debug line").unwrap();
    let error = body.find("an error line").unwrap();
    let info = body.find("an info line").unwrap();
    assert!(
        debug < error && error < info,
        "ascending level order should be DEBUG, ERROR, INFO"
    );
    // the sorted column's heading shows the direction and offers to flip it
    assert!(
        body.contains("(asc)"),
        "the sorted column shows its direction"
    );
    assert!(
        body.contains("/logs?sort=level&amp;dir=desc"),
        "clicking the sorted column flips the direction"
    );

    // sorted by level, descending: INFO, ERROR, DEBUG
    let (_, body) = get(&format!("{base}/logs?sort=level&dir=desc")).await;
    let debug = body.find("a debug line").unwrap();
    let error = body.find("an error line").unwrap();
    let info = body.find("an info line").unwrap();
    assert!(
        info < error && error < debug,
        "descending level order should be INFO, ERROR, DEBUG"
    );

    // an unknown sort falls back to newest first
    let (_, body) = get(&format!("{base}/logs?sort=nonsense")).await;
    assert!(body.contains("(desc)"), "the fallback sort is newest first");
}

/// Insert one log line per level, oldest first.
async fn insert_lines_per_level(dir: &std::path::Path) {
    for (n, (level, message)) in [
        ("DEBUG", "a debug line"),
        ("INFO", "an info line"),
        ("WARNING", "a warning line"),
        ("ERROR", "an error line"),
    ]
    .iter()
    .enumerate()
    {
        insert_log(
            dir,
            &format!("2026-10-22 16:05:0{n} -05:00"),
            level,
            message,
        )
        .await;
    }
}

/// The /logs page's level pills filter the lines: only the selected levels
/// render, and each pill links to the view with that level flipped.
#[tokio::test]
async fn the_logs_page_filters_lines_by_level() {
    let dir = common::tmp_dir();
    let base = spawn_ui(&dir).await;

    insert_lines_per_level(&dir).await;

    // every level by default, every pill on
    let (status, body) = get(&format!("{base}/logs")).await;
    assert_eq!(status, 200);
    for message in [
        "a debug line",
        "an info line",
        "a warning line",
        "an error line",
    ] {
        assert!(body.contains(message), "the default view shows {message:?}");
    }
    assert!(
        !body.contains("badge off"),
        "every pill is on when no level is filtered out"
    );
    assert!(
        body.contains("<a href=\"/logs?levels=&amp;sort=ts&amp;dir=desc\" title=\"Hide every level\" class=\"badge all\">All</a>"),
        "the All pill is on and hides every level: {body}"
    );
    // clicking it unchecks every level: the page it links to shows nothing
    let (_, body) = get(&format!("{base}/logs?levels=&sort=ts&dir=desc")).await;
    assert!(
        !body.contains("an info line"),
        "the All pill's view has no level selected"
    );
    assert!(
        body.contains("No log entries at the selected levels."),
        "the All pill's view says why it is empty"
    );

    // only WARNING and ERROR
    let (status, body) = get(&format!("{base}/logs?levels=WARNING,ERROR")).await;
    assert_eq!(status, 200);
    assert!(body.contains("a warning line"), "WARNING is selected");
    assert!(body.contains("an error line"), "ERROR is selected");
    assert!(!body.contains("a debug line"), "DEBUG is filtered out");
    assert!(!body.contains("an info line"), "INFO is filtered out");
    // the selected levels' pills keep their color, the hidden ones are
    // outlines of it
    assert!(
        body.contains("<a href=\"/logs?levels=ERROR&amp;sort=ts&amp;dir=desc\" title=\"Hide WARNING lines\" class=\"badge warning\">WARNING</a>"),
        "the WARNING pill is on and hides WARNING: {body}"
    );
    assert!(
        body.contains("<a href=\"/logs?levels=DEBUG,WARNING,ERROR&amp;sort=ts&amp;dir=desc\" title=\"Show DEBUG lines\" class=\"badge off debug\">DEBUG</a>"),
        "the DEBUG pill is off and shows DEBUG: {body}"
    );
    assert!(
        body.contains("<a href=\"/logs?levels=INFO,WARNING,ERROR&amp;sort=ts&amp;dir=desc\" title=\"Show INFO lines\" class=\"badge off info\">INFO</a>"),
        "the INFO pill is off and shows INFO: {body}"
    );
    // the All pill checks every level back on, keeping the sort
    assert!(
        body.contains("<a href=\"/logs?sort=ts&amp;dir=desc\" title=\"Show every level\" class=\"badge off all\">All</a>"),
        "the All pill is off and shows every level: {body}"
    );
    // the sort links keep the filter
    assert!(
        body.contains("/logs?levels=WARNING,ERROR&amp;sort=level&amp;dir=desc"),
        "the sort links keep the level filter"
    );

    // the filter composes with the sort
    let (status, body) = get(&format!(
        "{base}/logs?levels=WARNING,ERROR&sort=level&dir=asc"
    ))
    .await;
    assert_eq!(status, 200);
    let error = body.find("an error line").unwrap();
    let warning = body.find("a warning line").unwrap();
    assert!(error < warning, "ERROR sorts before WARNING ascending");

    // unknown level names are dropped; the rest still apply
    let (_, body) = get(&format!("{base}/logs?levels=TRACE,INFO")).await;
    assert!(body.contains("an info line"));
    assert!(!body.contains("a debug line"));
    assert!(!body.contains("a warning line"));
    assert!(!body.contains("an error line"));

    // level names are case-insensitive
    let (_, body) = get(&format!("{base}/logs?levels=error")).await;
    assert!(body.contains("an error line"));
    assert!(!body.contains("an info line"));
}

/// Unchecking every level shows nothing, and the page says why.
#[tokio::test]
async fn the_logs_page_with_no_level_selected_shows_no_lines() {
    let dir = common::tmp_dir();
    let base = spawn_ui(&dir).await;

    insert_lines_per_level(&dir).await;

    let (status, body) = get(&format!("{base}/logs?levels=")).await;
    assert_eq!(status, 200);
    assert!(!body.contains("a debug line"), "no level is selected");
    assert!(!body.contains("an info line"), "no level is selected");
    assert!(!body.contains("a warning line"), "no level is selected");
    assert!(!body.contains("an error line"), "no level is selected");
    assert!(
        body.contains("No log entries at the selected levels."),
        "the empty view says why it is empty"
    );
    for (level, class) in [
        ("DEBUG", "debug"),
        ("INFO", "info"),
        ("WARNING", "warning"),
        ("ERROR", "error"),
    ] {
        assert!(
            body.contains(&format!(
                "<a href=\"/logs?levels={level}&amp;sort=ts&amp;dir=desc\" title=\"Show {level} lines\" class=\"badge off {class}\">{level}</a>"
            )),
            "the {level} pill is off and shows {level}"
        );
    }
    // the All pill checks every level back on
    assert!(
        body.contains("<a href=\"/logs?sort=ts&amp;dir=desc\" title=\"Show every level\" class=\"badge off all\">All</a>"),
        "the All pill is off and shows every level again"
    );
}

#[tokio::test]
async fn posting_clear_log_empties_the_database() {
    let dir = common::tmp_dir();
    let base = spawn_ui(&dir).await;

    insert_log(
        &dir,
        "2026-10-22 16:05:07 -05:00",
        "INFO",
        "a line to clear",
    )
    .await;

    let resp = client()
        .post(format!("{base}/logs/clear"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 303, "the POST must redirect");
    assert_eq!(resp.headers().get("Location").unwrap(), "/logs");

    let config = common::test_config(&dir, "example.social");
    let db = fedifetcher::db::open(&config).await.unwrap();
    let rows = fedifetcher::db::recent_logs(
        &db,
        10,
        fedifetcher::db::LogSort::Time,
        fedifetcher::db::LogOrder::Desc,
        None,
    )
    .await
    .unwrap();
    assert!(rows.is_empty(), "the clear button empties the database");

    let (status, body) = get(&format!("{base}/logs")).await;
    assert_eq!(status, 200);
    assert!(body.contains("No log entries yet."));
}

/// Seed the seen_hosts table with the given entries, as a run would.
async fn seed_hosts(dir: &std::path::Path, hosts: serde_json::Value) {
    let config = common::test_config(dir, "example.social");
    let pool = fedifetcher::db::open(&config).await.unwrap();
    let mut state = fedifetcher::store::State::default();
    for (host, info) in hosts.as_object().unwrap() {
        state.seen_hosts_mut().add(host, info.clone());
    }
    fedifetcher::db::save_state(&pool, &state, fedifetcher::store::MAX_ENTRIES)
        .await
        .unwrap();
}

/// A seen_hosts entry that survives loading: the PeerTube marker expire()
/// requires of every entry, and a last_checked `days_ago` days back.
fn host_entry(software: &str, version: &str, apis: &[&str], days_ago: i64) -> serde_json::Value {
    let last_checked = fedifetcher::timeutil::fmt_state_ts(
        fedifetcher::timeutil::now() - time::Duration::days(days_ago),
    );
    let mut entry = serde_json::json!({
        "info": {},
        "software": software,
        "version": version,
        "peertubeApiSupport": false,
        "last_checked": last_checked,
    });
    for api in apis {
        entry
            .as_object_mut()
            .unwrap()
            .insert(format!("{api}ApiSupport"), serde_json::json!(true));
    }
    entry
}

#[tokio::test]
async fn the_hosts_page_sorts_by_the_clicked_column() {
    let dir = common::tmp_dir();
    seed_hosts(
        &dir,
        serde_json::json!({
            "alpha.example": host_entry("Mastodon", "4.2.0", &["mastodon"], 3),
            "beta.example": host_entry("Akkoma", "3.13.0", &["mastodon"], 1),
            "gamma.example": host_entry("Pleroma", "4.1.0", &[], 2),
        }),
    )
    .await;
    let base = spawn_ui(&dir).await;

    let position = |body: &str, host: &str| body.find(host).unwrap();

    // the default is hosts alphabetically
    let (status, body) = get(&format!("{base}/data")).await;
    assert_eq!(status, 200);
    assert!(
        position(&body, "alpha.example") < position(&body, "beta.example")
            && position(&body, "beta.example") < position(&body, "gamma.example"),
        "hosts sort alphabetically by default"
    );
    assert!(
        body.contains("<td>mastodon</td>"),
        "the APIs column lists the supported APIs"
    );
    assert!(
        body.contains("/data?tab=hosts&amp;sort=software&amp;dir=asc"),
        "a new column sorts the page's default way"
    );

    // by software, ascending: Akkoma, Mastodon, Pleroma
    let (_, body) = get(&format!("{base}/data?sort=software&dir=asc")).await;
    assert!(
        position(&body, "beta.example") < position(&body, "alpha.example")
            && position(&body, "alpha.example") < position(&body, "gamma.example"),
        "software ascending is Akkoma, Mastodon, Pleroma"
    );
    assert!(
        body.contains("(asc)"),
        "the sorted column shows its direction"
    );
    assert!(
        body.contains("/data?tab=hosts&amp;sort=software&amp;dir=desc"),
        "clicking the sorted column flips the direction"
    );

    // by software, descending: Pleroma, Mastodon, Akkoma
    let (_, body) = get(&format!("{base}/data?sort=software&dir=desc")).await;
    assert!(
        position(&body, "gamma.example") < position(&body, "alpha.example")
            && position(&body, "alpha.example") < position(&body, "beta.example"),
        "software descending is Pleroma, Mastodon, Akkoma"
    );

    // by version, ascending: 3.13.0, 4.1.0, 4.2.0
    let (_, body) = get(&format!("{base}/data?sort=version&dir=asc")).await;
    assert!(
        position(&body, "beta.example") < position(&body, "gamma.example")
            && position(&body, "gamma.example") < position(&body, "alpha.example"),
        "version ascending is 3.13.0, 4.1.0, 4.2.0"
    );

    // by last checked, descending: 1, 2, 3 days ago
    let (_, body) = get(&format!("{base}/data?sort=last_checked&dir=desc")).await;
    assert!(
        position(&body, "beta.example") < position(&body, "gamma.example")
            && position(&body, "gamma.example") < position(&body, "alpha.example"),
        "the most recently checked host comes first"
    );

    // an unknown sort falls back to hosts alphabetically
    let (_, body) = get(&format!("{base}/data?sort=nonsense")).await;
    assert!(
        position(&body, "alpha.example") < position(&body, "beta.example")
            && position(&body, "beta.example") < position(&body, "gamma.example"),
        "an unknown sort falls back to the host column"
    );
}

/// Seed the database with rows in every table, as runs would.
async fn seed_db(dir: &std::path::Path) {
    let config = common::test_config(dir, "example.social");
    let pool = fedifetcher::db::open(&config).await.unwrap();
    sqlx::query(
        "INSERT INTO oauth_accounts (token, server, acct, avatar, connected_at)
         VALUES ('tok', 'example.social', 'me', '', '2026-01-01')",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO oauth_apps (server, client_id, client_secret, registered_at)
         VALUES ('example.social', 'id', 'secret', '2026-01-01')",
    )
    .execute(&pool)
    .await
    .unwrap();
    let log = |level: &str, message: &str| fedifetcher::db::LogRow {
        ts: "2026-10-22 16:05:07 -05:00".to_string(),
        level: level.to_string(),
        target: "fedifetcher::test".to_string(),
        message: message.to_string(),
    };
    fedifetcher::db::insert_log(&pool, &log("ERROR", "first line"))
        .await
        .unwrap();
    fedifetcher::db::insert_log(&pool, &log("INFO", "second line"))
        .await
        .unwrap();
    let run =
        |started: &str, finished: &str, outcome: &str, message: &str| fedifetcher::db::RunRow {
            started: started.to_string(),
            finished: finished.to_string(),
            outcome: outcome.to_string(),
            message: message.to_string(),
        };
    fedifetcher::db::insert_run(
        &pool,
        &run(
            "2026-10-22 16:05:07.000001-05:00",
            "2026-10-22 16:05:08.000001-05:00",
            "error",
            "first run",
        ),
    )
    .await
    .unwrap();
    fedifetcher::db::insert_run(
        &pool,
        &run(
            "2026-10-22 16:05:09.000001-05:00",
            "2026-10-22 16:05:10.000001-05:00",
            "ok",
            "second run",
        ),
    )
    .await
    .unwrap();
    fedifetcher::db::mark_private_post(&pool, "https://remote.example/@a/1")
        .await
        .unwrap();
    fedifetcher::db::mark_opted_out_account(&pool, "bot@remote.example", "it is not indexable")
        .await
        .unwrap();
}

#[tokio::test]
async fn the_data_page_drills_into_the_databases_tables() {
    let dir = common::tmp_dir();
    seed_hosts(
        &dir,
        serde_json::json!({
            "alpha.example": host_entry("Mastodon", "4.2.0", &["mastodon"], 3),
        }),
    )
    .await;
    seed_db(&dir).await;
    let base = spawn_ui(&dir).await;

    let position = |body: &str, needle: &str| body.find(needle).unwrap();

    // every table but logs and config is a sub-tab, with its row count;
    // those two tables have pages of their own
    let (status, body) = get(&format!("{base}/data")).await;
    assert_eq!(status, 200);
    for tab in [
        "hosts",
        "opted_out_accounts",
        "private_posts",
        "runs",
        "seen_urls",
    ] {
        assert!(
            body.contains(&format!("href=\"/data?tab={tab}\"")),
            "the {tab} sub-tab is linked"
        );
    }
    for gone in ["logs", "config", "oauth_accounts", "oauth_apps"] {
        assert!(
            !body.contains(&format!("href=\"/data?tab={gone}\"")),
            "the {gone} table has no sub-tab"
        );
    }
    // asking for an OAuth tab by name falls back to the hosts tab, whatever
    // rows the tables hold
    let (status, body) = get(&format!("{base}/data?tab=oauth_accounts")).await;
    assert_eq!(status, 200);
    assert!(
        body.contains("Seen hosts"),
        "an OAuth table's name falls back to the hosts tab"
    );
    assert!(body.contains("(2)"), "a sub-tab shows its row count");
    // the hosts tab is the default
    assert!(body.contains("Seen hosts"));
    assert!(body.contains("alpha.example"));

    // a table tab shows the table's rows, newest first by default
    let (status, body) = get(&format!("{base}/data?tab=runs")).await;
    assert_eq!(status, 200);
    assert!(body.contains("first run"));
    assert!(body.contains("second run"));
    assert!(
        position(&body, "second run") < position(&body, "first run"),
        "the newest row comes first by default"
    );
    assert!(
        body.contains("/data?tab=runs&amp;sort=outcome&amp;dir=asc"),
        "a column heading links to that sort"
    );

    // sorted by a column, ascending: error before ok
    let (_, body) = get(&format!("{base}/data?tab=runs&sort=outcome&dir=asc")).await;
    assert!(
        position(&body, "first run") < position(&body, "second run"),
        "outcome ascending is error, ok"
    );
    // and descending flips it
    let (_, body) = get(&format!("{base}/data?tab=runs&sort=outcome&dir=desc")).await;
    assert!(
        position(&body, "second run") < position(&body, "first run"),
        "outcome descending is ok, error"
    );
    // an unknown column falls back to the default sort, newest first
    let (_, body) = get(&format!("{base}/data?tab=runs&sort=nonsense")).await;
    assert!(
        position(&body, "second run") < position(&body, "first run"),
        "an unknown column falls back to the newest first"
    );

    // the other tables show their rows too
    let (status, body) = get(&format!("{base}/data?tab=private_posts")).await;
    assert_eq!(status, 200);
    assert!(body.contains("https://remote.example/@a/1"));
    let (status, body) = get(&format!("{base}/data?tab=opted_out_accounts")).await;
    assert_eq!(status, 200);
    assert!(body.contains("bot@remote.example"));
    assert!(body.contains("it is not indexable"));
    // the row offers to backfill the account anyway, past its opt-out
    assert!(body.contains("name=\"kind\" value=\"user\""));
    assert!(body.contains("name=\"set\" value=\"checked\""));
    assert!(
        body.contains("name=\"force\" value=\"1\""),
        "the button forces past the opt-out"
    );
    assert!(body.contains("name=\"key\" value=\"bot@remote.example\""));
    assert!(body.contains(">Force backfill</button>"));

    // an unknown tab falls back to the hosts tab, and so do the tables
    // with pages of their own
    for tab in ["nonsense", "logs", "config"] {
        let (status, body) = get(&format!("{base}/data?tab={tab}")).await;
        assert_eq!(status, 200);
        assert!(
            body.contains("Seen hosts"),
            "{tab} falls back to the hosts tab"
        );
    }
}

#[tokio::test]
async fn posting_stop_redirects_back_even_when_idle() {
    let dir = common::tmp_dir();
    let base = spawn_ui(&dir).await;

    let resp = client().post(format!("{base}/stop")).send().await.unwrap();
    assert_eq!(resp.status().as_u16(), 303, "the POST must redirect");
    assert_eq!(resp.headers().get("Location").unwrap(), "/");

    // idle: the top bar offers to run, not to stop
    let (status, body) = get(&format!("{base}/")).await;
    assert_eq!(status, 200);
    assert!(body.contains(">Start</button>"));
    assert!(!body.contains(">Stop</button>"));
}

#[tokio::test]
async fn the_stop_button_stops_a_running_run() {
    // the start callback answers slowly, so the run is observably running
    let server = MockServer::start_async().await;
    server
        .mock_async(|when, then| {
            when.method(Method::GET).path("/start");
            then.status(200).delay(std::time::Duration::from_secs(3));
        })
        .await;
    let dir = common::tmp_dir();
    let mut config = common::test_config(&dir, "example.social");
    config.on_start = Some(format!("{}/start", server.base_url()));
    let (base, _) = spawn_ui_with(config).await;

    let resp = client().post(format!("{base}/run")).send().await.unwrap();
    assert_eq!(resp.status().as_u16(), 303);

    // while the run is going, the top bar offers to stop it
    let running_page = page_containing(&base, "/", &["badge running\">Running<"]).await;
    assert!(
        running_page.contains("badge running\">Running<"),
        "the run should be running"
    );
    assert!(running_page.contains(">Stop</button>"));
    assert!(!running_page.contains(">Start</button>"));

    let resp = client().post(format!("{base}/stop")).send().await.unwrap();
    assert_eq!(resp.status().as_u16(), 303);
    assert_eq!(resp.headers().get("Location").unwrap(), "/");

    // the run finishes early, and the history records that it was stopped,
    // along with how long it ran before stopping
    let body = page_containing(&base, "/", &["Stopped after "]).await;
    assert!(
        body.contains("Stopped after "),
        "the run should report being stopped, with its elapsed time"
    );
    assert!(
        body.contains(">stopped<"),
        "the outcome badge should say stopped"
    );
    assert!(body.contains(">Idle<"), "the run should be over");
}

#[tokio::test]
async fn the_pause_button_pauses_a_running_run_and_resume_continues_it() {
    // the start callback answers slowly, so the run is observably in
    // progress past the moment it is paused
    let server = MockServer::start_async().await;
    server
        .mock_async(|when, then| {
            when.method(Method::GET).path("/start");
            then.status(200).delay(std::time::Duration::from_secs(2));
        })
        .await;
    let dir = common::tmp_dir();
    let mut config = common::test_config(&dir, "example.social");
    config.on_start = Some(format!("{}/start", server.base_url()));
    let (base, _) = spawn_ui_with(config).await;

    let resp = client().post(format!("{base}/run")).send().await.unwrap();
    assert_eq!(resp.status().as_u16(), 303);

    // while the run is going, the top bar offers to pause it
    let running_page = page_containing(&base, "/", &["badge running\">Running<"]).await;
    assert!(
        running_page.contains("badge running\">Running<"),
        "the run should be running"
    );
    assert!(running_page.contains(">Pause</button>"));
    assert!(running_page.contains(">Stop</button>"));
    assert!(!running_page.contains(">Resume</button>"));

    let resp = client().post(format!("{base}/pause")).send().await.unwrap();
    assert_eq!(resp.status().as_u16(), 303);
    assert_eq!(resp.headers().get("Location").unwrap(), "/");

    // the dashboard shows the pause: the badge, and a resume button in
    // place of the pause one — stop stays available
    let paused_page = page_containing(&base, "/", &["badge paused"]).await;
    assert!(
        paused_page.contains("badge paused"),
        "the status badge should say paused"
    );
    assert!(paused_page.contains(">Resume</button>"));
    assert!(paused_page.contains(">Stop</button>"));
    assert!(!paused_page.contains(">Pause</button>"));

    // the callback has finished by now, and the run would have finished
    // with it — a paused run holds instead
    tokio::time::sleep(std::time::Duration::from_millis(2800)).await;
    let (_, page) = get(&format!("{base}/")).await;
    assert!(
        !page.contains("Processing finished in"),
        "a paused run holds past the point it would finish"
    );
    assert!(page.contains("badge paused"));

    let resp = client()
        .post(format!("{base}/resume"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 303);
    assert_eq!(resp.headers().get("Location").unwrap(), "/");

    // the resume sets the run going again: the run shows as running, or
    // has already finished — it was holding at its last checkpoint, so
    // finishing is what the resume set loose. The poll waits for a
    // consistent page: the dashboard reads the status and the run history
    // at two moments, so a page can briefly show the finished status next
    // to the still-running history row.
    let resumed_page = page_containing(
        &base,
        "/",
        &["badge running\">Running<", "Processing finished in"],
    )
    .await;
    assert!(
        !resumed_page.is_empty(),
        "the resume should set the run going again"
    );
    assert!(!resumed_page.contains(">Resume</button>"));

    // and the run finishes once it is going again
    let body = page_containing(&base, "/", &["Processing finished in"]).await;
    assert!(
        body.contains("Processing finished in"),
        "the resumed run should finish"
    );
    assert!(body.contains(">Idle<"), "the run should be over");
}

#[tokio::test]
async fn saving_the_config_page_writes_the_database() {
    let dir = common::tmp_dir();
    let mut config = common::test_config(&dir, "");
    config.access_tokens = Vec::new();
    let (base, pool) = spawn_ui_with(config).await;

    let (status, location, _) = post_config(
        &base,
        "server=example.social&new_access_token=smoke-secret-token-123&max_followers=5&from_lists=1&respect_robots_txt=0&http_timeout=9&hide_server_in_user_agent=1&log_page_loads=1&streaming=1",
    )
    .await;
    assert_eq!(status, 303, "the POST must redirect");
    assert_eq!(location.as_deref(), Some("/config"));

    let saved = saved_config(&pool).await;
    assert_eq!(saved.server, "example.social");
    assert_eq!(saved.access_tokens, vec![TOKEN.to_string()]);
    assert_eq!(saved.max_followers, 5);
    assert!(saved.from_lists);
    assert!(!saved.respect_robots_txt);
    assert_eq!(saved.http_timeout, 9);
    assert!(
        saved.hide_server_in_user_agent,
        "the page saves the user agent option"
    );
    assert!(
        saved.log_page_loads,
        "the page saves the page-load logging option"
    );
    assert!(saved.streaming, "the page saves the streaming option");

    // the page shows the saved values, and still masks the token
    let (status, body) = get(&format!("{base}/config")).await;
    assert_eq!(status, 200);
    assert!(body.contains("example.social"));
    assert!(body.contains(&masked(TOKEN)));
    assert!(!body.contains(TOKEN), "the token itself must not appear");
    assert!(
        body.contains("name=\"log_page_loads\""),
        "the page shows the page-load logging option"
    );
}

/// Page loads are logged only when the config's toggle says so: off by
/// default, on it logs one DEBUG line per GET.
#[tokio::test]
async fn page_loads_are_logged_only_when_the_config_says_so() {
    let dir = common::tmp_dir();
    let base = spawn_ui(&dir).await;
    install_page_load_capture();

    // off by default: loading a page logs nothing
    let (status, _) = get(&format!("{base}/logs")).await;
    assert_eq!(status, 200);
    let lines = page_load_lines();
    assert!(
        !lines.iter().any(|line| line.contains("GET /logs")),
        "no page load is logged by default:\n{}",
        lines.join("\n")
    );

    // the config page's toggle turns the logging on
    let (status, _, _) = post_config(&base, "log_page_loads=1").await;
    assert_eq!(status, 303);
    let (status, _) = get(&format!("{base}/logs")).await;
    assert_eq!(status, 200);
    let lines = page_load_lines();
    assert!(
        lines.iter().any(|line| line.contains("GET /logs")),
        "a page load is logged once the toggle is on:\n{}",
        lines.join("\n")
    );
}

#[tokio::test]
async fn removing_a_token_via_the_config_page() {
    let dir = common::tmp_dir();
    let mut config = common::test_config(&dir, "example.social");
    config.access_tokens = vec![
        "first-token-aaaa".to_string(),
        "second-token-bbbb".to_string(),
    ];
    let (base, pool) = spawn_ui_with(config).await;

    let (status, _, _) = post_config(&base, "remove_access_token=0").await;
    assert_eq!(status, 303);

    let saved = saved_config(&pool).await;
    assert_eq!(saved.access_tokens, vec!["second-token-bbbb".to_string()]);
}

#[tokio::test]
async fn invalid_values_are_rejected_and_not_saved() {
    let dir = common::tmp_dir();
    let mut config = common::test_config(&dir, "example.social");
    config.access_tokens = vec![TOKEN.to_string()];
    let (base, pool) = spawn_ui_with(config).await;

    let (status, _, body) = post_config(&base, "http_timeout=soon").await;
    assert_eq!(status, 400);
    assert!(body.contains("http_timeout"), "the error names the option");

    assert!(
        fedifetcher::db::load_config(&pool)
            .await
            .unwrap()
            .is_empty(),
        "a failed save must not write the config"
    );
    let (_, body) = get(&format!("{base}/config")).await;
    assert!(!body.contains("soon"), "the config must be unchanged");
}

#[tokio::test]
async fn the_debug_page_names_the_server_and_links_in_the_nav() {
    let dir = common::tmp_dir();
    let base = spawn_ui(&dir).await;

    let (status, body) = get(&format!("{base}/debug")).await;
    assert_eq!(status, 200);
    assert!(body.contains("Test connection"));
    assert!(body.contains("Look up a user"));
    assert!(body.contains("Recent posts"));
    assert!(
        body.contains("example.social"),
        "names the configured server"
    );
    assert!(body.contains(&masked(TOKEN)), "names the token, masked");
    assert!(body.contains("href=\"/debug\""), "the nav links to the tab");
    assert!(!body.contains("<script"), "must not contain JavaScript");
}

#[tokio::test]
async fn debug_actions_need_a_server_configured() {
    let dir = common::tmp_dir();
    let mut config = common::test_config(&dir, "");
    config.access_tokens = Vec::new();
    let (base, _) = spawn_ui_with(config).await;

    let (status, body) = post_debug(&base, "action=connect").await;
    assert_eq!(status, 200);
    assert!(body.contains("No server is configured"), "{body}");
    assert!(body.contains("failed"), "the result is a failure");
}

#[tokio::test]
async fn debug_actions_need_a_token_configured() {
    let dir = common::tmp_dir();
    let mut config = common::test_config(&dir, "example.social");
    config.access_tokens = Vec::new();
    let (base, _) = spawn_ui_with(config).await;

    let (status, body) = post_debug(&base, "action=connect").await;
    assert_eq!(status, 200);
    assert!(body.contains("No access token is configured"), "{body}");
}

#[tokio::test]
async fn debug_user_actions_need_an_account() {
    let dir = common::tmp_dir();
    let base = spawn_ui(&dir).await;

    for form in ["action=user&user=", "action=posts&user="] {
        let (status, body) = post_debug(&base, form).await;
        assert_eq!(status, 200, "{form}");
        assert!(body.contains("Enter an account"), "{form}: {body}");
    }
}

#[tokio::test]
async fn debug_rejects_an_unknown_action() {
    let dir = common::tmp_dir();
    let base = spawn_ui(&dir).await;

    let (status, body) = post_debug(&base, "action=dance").await;
    assert_eq!(status, 200);
    assert!(body.contains("Unknown action"), "{body}");
}

/// The Test connection form offers every configured identity: OAuth
/// accounts by their acct and their own server, pasted tokens by their
/// masked value and the configured server. No token ever reaches the page.
#[tokio::test]
async fn the_debug_page_offers_each_identity_to_test_with() {
    let dir = common::tmp_dir();
    let mut config = common::test_config(&dir, "example.social");
    config.access_tokens = vec![TOKEN.to_string(), OAUTH_TOKEN.to_string()];
    let (base, db) = spawn_ui_with(config).await;
    fedifetcher::db::save_oauth_account(&db, OAUTH_TOKEN, "other.example", "someone", "")
        .await
        .unwrap();

    let (status, body) = get(&format!("{base}/debug")).await;
    assert_eq!(status, 200);
    assert!(body.contains("name=\"token_index\""), "{body}");
    assert!(body.contains("@someone"), "the account's label: {body}");
    assert!(
        body.contains("other.example"),
        "the account's own server: {body}"
    );
    assert!(
        body.contains(&masked(TOKEN)),
        "the pasted token's label: {body}"
    );
    assert!(
        !body.contains(TOKEN) && !body.contains(OAUTH_TOKEN),
        "no full token in the page: {body}"
    );
}

/// Testing a connected account asks that account's own server, not the
/// configured one, with that account's token.
#[tokio::test]
async fn the_connection_test_uses_the_picked_identitys_server() {
    let server = MockServer::start_async().await;
    let mock_host = server.base_url().trim_start_matches("http://").to_string();
    let instance = server
        .mock_async(|when, then| {
            when.method(Method::GET).path("/api/v1/instance");
            then.status(200).json_body(
                serde_json::json!({"title": "Other", "uri": mock_host, "version": "4.2.0"}),
            );
        })
        .await;
    let verify = server
        .mock_async(|when, then| {
            when.method(Method::GET)
                .path("/api/v1/accounts/verify_credentials")
                .header("Authorization", "Bearer the-oauth-token");
            then.status(200).json_body(
                serde_json::json!({"id": "1", "acct": "someone", "display_name": "Someone"}),
            );
        })
        .await;
    let dir = common::tmp_dir();
    let mut config = common::test_config(&dir, "example.social");
    config.access_tokens = vec![TOKEN.to_string(), "the-oauth-token".to_string()];
    let db = fedifetcher::db::open(&config).await.unwrap();
    fedifetcher::db::save_oauth_account(&db, "the-oauth-token", &mock_host, "someone", "")
        .await
        .unwrap();
    let app = web::app_with_scheme(config, db.clone(), "http");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });
    let base = format!("http://{addr}");

    let (status, body) = post_debug(&base, "action=connect&token_index=1").await;
    assert_eq!(status, 200);
    assert!(body.contains("Other"), "the account's instance: {body}");
    assert!(body.contains("someone"), "the account: {body}");
    assert_eq!(instance.calls(), 1);
    assert_eq!(verify.calls(), 1);
}

/// A picker index that no longer matches a token is a failure, not a
/// silently different test.
#[tokio::test]
async fn the_connection_test_rejects_an_unknown_picker_index() {
    let dir = common::tmp_dir();
    let base = spawn_ui(&dir).await;

    let (status, body) = post_debug(&base, "action=connect&token_index=7").await;
    assert_eq!(status, 200);
    assert!(body.contains("not configured any more"), "{body}");
}

/// After a test run, the picker still shows the identity that was tested,
/// not the first one.
#[tokio::test]
async fn the_picker_keeps_showing_the_tested_identity() {
    let dir = common::tmp_dir();
    let mut config = common::test_config(&dir, "example.social");
    config.access_tokens = vec![TOKEN.to_string(), OAUTH_TOKEN.to_string()];
    let (base, db) = spawn_ui_with(config).await;
    fedifetcher::db::save_oauth_account(&db, OAUTH_TOKEN, "other.example", "someone", "")
        .await
        .unwrap();

    let (status, body) = post_debug(&base, "action=connect&token_index=1").await;
    assert_eq!(status, 200);
    let option = body
        .split("<option ")
        .find(|tag| tag.starts_with("value=\"1\""))
        .unwrap_or_else(|| panic!("an option for the tested identity: {body}"));
    assert!(
        option.contains("selected"),
        "the tested identity stays selected: {body}"
    );
}

/// The logs page refreshes itself while live updates are on — a meta
/// refresh, the one kind of live update that needs no JavaScript — and
/// the top bar's toggle pauses and resumes it with a cookie, going back
/// to the page it was clicked on.
#[tokio::test]
async fn the_logs_page_refreshes_itself_unless_paused() {
    let dir = common::tmp_dir();
    let base = spawn_ui(&dir).await;

    let (status, body) = get(&format!("{base}/logs")).await;
    assert_eq!(status, 200);
    assert!(body.contains("http-equiv=\"refresh\""), "live by default");
    assert!(body.contains("content=\"5\""), "every five seconds");
    assert!(
        body.contains("href=\"/live?on=0\"") && body.contains(">Pause live updates</a>"),
        "the top bar's toggle pauses: {body}"
    );

    // the toggle is in the top bar, on every page
    let (status, body) = get(&format!("{base}/")).await;
    assert_eq!(status, 200);
    assert!(
        body.contains("href=\"/live?on=0\"") && body.contains(">Pause live updates</a>"),
        "the toggle is in the top bar: {body}"
    );

    // pausing sets the cookie and goes back to the page it was clicked
    // on, sort and all
    let resp = client()
        .get(format!("{base}/live?on=0"))
        .header("Referer", format!("{base}/logs?sort=level&dir=asc"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 303, "the toggle redirects back");
    assert_eq!(
        resp.headers().get("Location").unwrap(),
        "/logs?sort=level&dir=asc",
        "back to the page, the sort kept"
    );
    let cookie = resp.headers().get("Set-Cookie").unwrap().to_str().unwrap();
    assert!(
        cookie.starts_with("ff_live=0;"),
        "the cookie pauses: {cookie}"
    );

    // with the cookie, the page does not refresh and offers to resume
    let resp = client()
        .get(format!("{base}/logs"))
        .header("Cookie", "ff_live=0")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    let body = resp.text().await.unwrap();
    assert!(
        !body.contains("http-equiv=\"refresh\""),
        "paused: no auto-refresh"
    );
    assert!(
        body.contains("href=\"/live?on=1\"") && body.contains(">Resume live updates</a>"),
        "the toggle resumes: {body}"
    );

    // resuming with no page to go back to lands on the dashboard
    let resp = client()
        .get(format!("{base}/live?on=1"))
        .header("Cookie", "ff_live=0")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 303);
    assert_eq!(resp.headers().get("Location").unwrap(), "/");
    let cookie = resp.headers().get("Set-Cookie").unwrap().to_str().unwrap();
    assert!(
        cookie.starts_with("ff_live=1;"),
        "the cookie resumes: {cookie}"
    );
}

/// A dashboard refreshes itself while a run is in progress, even with
/// live updates paused: the run's elapsed time must tick and its row
/// must flip to finished without the operator watching. With no run in
/// progress, the top bar's toggle still governs the dashboard.
#[tokio::test]
async fn the_dashboard_refreshes_while_a_run_is_in_progress_even_when_paused() {
    let dir = common::tmp_dir();
    let config = common::test_config(&dir, "example.social");
    let (base, db) = spawn_ui_with(config).await;

    // no run in progress: a paused dashboard does not refresh
    let resp = client()
        .get(format!("{base}/"))
        .header("Cookie", "ff_live=0")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    let body = resp.text().await.unwrap();
    assert!(
        !body.contains("http-equiv=\"refresh\""),
        "paused and idle: no auto-refresh"
    );

    // a run recorded as started but not finished, as one in progress is
    let started = fedifetcher::timeutil::fmt_state_ts(fedifetcher::timeutil::now());
    fedifetcher::db::start_run(&db, &started).await.unwrap();

    let resp = client()
        .get(format!("{base}/"))
        .header("Cookie", "ff_live=0")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    let body = resp.text().await.unwrap();
    assert!(
        body.contains("http-equiv=\"refresh\""),
        "a run in progress refreshes the dashboard, even when paused"
    );
    assert!(
        body.contains("href=\"/live?on=1\"") && body.contains(">Resume live updates</a>"),
        "the toggle is in the top bar: {body}"
    );
}

/// The data page moved from /hosts to /data: the old links redirect
/// permanently, keeping the query string.
#[tokio::test]
async fn the_old_hosts_links_redirect_to_the_data_page() {
    let dir = common::tmp_dir();
    let base = spawn_ui(&dir).await;

    for (old, new) in [
        ("/hosts", "/data"),
        (
            "/hosts?tab=runs&sort=outcome&dir=asc",
            "/data?tab=runs&sort=outcome&dir=asc",
        ),
    ] {
        let resp = client().get(format!("{base}{old}")).send().await.unwrap();
        assert_eq!(resp.status().as_u16(), 308, "{old} redirects permanently");
        assert_eq!(
            resp.headers().get("Location").unwrap(),
            new,
            "{old} goes to {new}"
        );
    }
}

/// The data page offers a fetch button on every row of the tables that
/// have one: posts fetch their context, users are backfilled, seen URLs
/// are re-resolved, hosts are re-checked.
#[tokio::test]
async fn the_data_page_offers_a_fetch_button_per_actionable_row() {
    let dir = common::tmp_dir();
    let config = common::test_config(&dir, "example.social");
    let pool = fedifetcher::db::open(&config).await.unwrap();

    fedifetcher::db::record_trigger_posts(
        &pool,
        &[fedifetcher::db::TriggerPost {
            uri: "https://remote.example/@a/1".to_string(),
            url: "https://remote.example/@a/1".to_string(),
            acct: Some("a@remote.example".to_string()),
            author_url: None,
            replies_count: Some(2),
            created_at: None,
            in_reply_to_id: None,
            in_reply_to_account_id: None,
            is_public: true,
            is_boost: false,
            mentions: None,
            source: "home timeline".to_string(),
        }],
        1000,
    )
    .await
    .unwrap();
    let mut state = fedifetcher::store::State::default();
    state
        .known_followings_mut()
        .add("alice@remote.example", None);
    state
        .recently_checked_users_mut()
        .add("bob@remote.example", None);
    state
        .seen_urls_mut()
        .add("https://remote.example/@a/9", None);
    state
        .recently_checked_context_mut()
        .mark_fetched("https://remote.example/@a/5", fedifetcher::timeutil::now());
    state.seen_hosts_mut().add(
        "alpha.example",
        host_entry("Mastodon", "4.2.0", &["mastodon"], 3),
    );
    fedifetcher::db::save_state(&pool, &state, fedifetcher::store::MAX_ENTRIES)
        .await
        .unwrap();
    let base = spawn_ui(&dir).await;

    // a post: fetch its context
    let (status, body) = get(&format!("{base}/data?tab=trigger_posts")).await;
    assert_eq!(status, 200);
    assert!(
        body.contains("action=\"/fetch\""),
        "the row posts to /fetch"
    );
    assert!(body.contains("name=\"kind\" value=\"post\""));
    assert!(
        body.contains("name=\"key\" value=\"https://remote.example/@a/1\""),
        "the post's uri is the key"
    );
    assert!(body.contains(">Fetch context</button>"));
    assert!(
        body.contains("name=\"back\" value=\"/data?tab=trigger_posts&amp;sort=uri&amp;dir=asc\""),
        "the button returns to this view"
    );

    // a context cache row: same action
    let (status, body) = get(&format!("{base}/data?tab=context_cache")).await;
    assert_eq!(status, 200);
    assert!(
        body.contains("name=\"key\" value=\"https://remote.example/@a/5\""),
        "the cached context post is the key"
    );

    // a known following: backfill the user, into the followings set
    let (status, body) = get(&format!("{base}/data?tab=known_followings")).await;
    assert_eq!(status, 200);
    assert!(body.contains("name=\"kind\" value=\"user\""));
    assert!(body.contains("name=\"set\" value=\"followings\""));
    assert!(body.contains("name=\"key\" value=\"alice@remote.example\""));
    assert!(body.contains(">Backfill user</button>"));

    // a recently checked user: backfill, into the checked set
    let (status, body) = get(&format!("{base}/data?tab=recently_checked_users")).await;
    assert_eq!(status, 200);
    assert!(body.contains("name=\"set\" value=\"checked\""));
    assert!(body.contains("name=\"key\" value=\"bob@remote.example\""));

    // a seen URL: re-resolve it
    let (status, body) = get(&format!("{base}/data?tab=seen_urls")).await;
    assert_eq!(status, 200);
    assert!(body.contains("name=\"kind\" value=\"url\""));
    assert!(body.contains("name=\"key\" value=\"https://remote.example/@a/9\""));
    assert!(body.contains(">Re-resolve</button>"));

    // a host: re-check its server info
    let (status, body) = get(&format!("{base}/data")).await;
    assert_eq!(status, 200);
    assert!(body.contains("name=\"kind\" value=\"host\""));
    assert!(body.contains("name=\"key\" value=\"alpha.example\""));
    assert!(body.contains(">Re-check</button>"));

    // a table without an action has no fetch form
    let (status, body) = get(&format!("{base}/data?tab=private_posts")).await;
    assert_eq!(status, 200);
    assert!(!body.contains("action=\"/fetch\""));
}

/// POST a urlencoded form to /fetch: (status, Location).
async fn post_fetch(base: &str, form: &str) -> (u16, Option<String>) {
    let resp = client()
        .post(format!("{base}/fetch"))
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(form.to_string())
        .send()
        .await
        .unwrap();
    let status = resp.status().as_u16();
    let location = resp
        .headers()
        .get("Location")
        .map(|value| value.to_str().unwrap().to_string());
    (status, location)
}

/// A fetch POST redirects back to the page the button was on; the fetch it
/// starts runs in the background.
#[tokio::test]
async fn posting_fetch_redirects_back_to_the_given_page() {
    let dir = common::tmp_dir();
    // a server that is not there, so the fetch the POST starts fails fast
    // instead of reaching out
    let mut config = common::test_config(&dir, "127.0.0.1:9");
    config.access_tokens = vec![TOKEN.to_string()];
    let (base, _) = spawn_ui_with(config).await;

    let (status, location) = post_fetch(
        &base,
        "kind=url&key=https%3A%2F%2Fremote.example%2F%40a%2F1&back=%2Fdata%3Ftab%3Dseen_urls",
    )
    .await;
    assert_eq!(status, 303, "the POST must redirect");
    assert_eq!(location.as_deref(), Some("/data?tab=seen_urls"));
}

/// The fetch POST's fields are validated: an unknown kind or an empty key
/// is not a fetch this UI offers, and the redirect target is always a
/// same-site path.
#[tokio::test]
async fn posting_fetch_validates_its_fields() {
    let dir = common::tmp_dir();
    let base = spawn_ui(&dir).await;

    // an unknown kind is not a fetch this UI offers
    let (status, location) = post_fetch(&base, "kind=dance&key=x&back=/data").await;
    assert_eq!(status, 303);
    assert_eq!(location.as_deref(), Some("/data"));

    // a fetch with no key does nothing
    let (status, location) = post_fetch(&base, "kind=post&key=&back=/data").await;
    assert_eq!(status, 303);
    assert_eq!(location.as_deref(), Some("/data"));

    // the redirect target is a same-site path, never off-site
    for form in [
        "kind=url&key=https://x.example/1&back=https://evil.example/",
        "kind=url&key=https://x.example/1&back=//evil.example",
        "kind=url&key=https://x.example/1&back=",
    ] {
        let (status, location) = post_fetch(&base, form).await;
        assert_eq!(status, 303, "{form}");
        assert_eq!(location.as_deref(), Some("/"), "{form}");
    }
}

/// A manual fetch is recorded in the run history like a run, and its
/// message says it was a manual fetch — not a scheduled run's "Processing
/// finished".
#[tokio::test]
async fn a_manual_fetch_is_recorded_in_the_run_history() {
    let dir = common::tmp_dir();
    // a server that is not there, so the fetch fails fast instead of
    // reaching out
    let mut config = common::test_config(&dir, "127.0.0.1:9");
    config.access_tokens = vec![TOKEN.to_string()];
    let (base, db) = spawn_ui_with(config).await;

    let (status, location) = post_fetch(
        &base,
        "kind=url&key=https%3A%2F%2Fremote.example%2F%40a%2F1&back=%2F",
    )
    .await;
    assert_eq!(status, 303, "the POST must redirect");
    assert_eq!(location.as_deref(), Some("/"));

    // the fetch runs in the background; its record lands when it ends
    let mut runs = Vec::new();
    for _ in 0..100 {
        runs = fedifetcher::db::recent_runs(&db, 10).await.unwrap();
        if runs.iter().any(|run| run.message.contains("Manual fetch")) {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert_eq!(runs.len(), 1, "the fetch should be the only run recorded");
    assert_eq!(runs[0].outcome, "ok");
    assert_eq!(
        runs[0].message, "Manual fetch: url https://remote.example/@a/1",
        "the message says it was a manual fetch, and of what"
    );

    // and the dashboard's history shows it
    let (status, body) = get(&format!("{base}/")).await;
    assert_eq!(status, 200);
    assert!(
        body.contains("Manual fetch: url https://remote.example/@a/1"),
        "the dashboard shows the manual fetch: {body}"
    );
}

/// The about page, which the top bar's version number links to: what
/// this is, and the license it is under. There is no alert box to open —
/// the UI is JavaScript-free by design — so the "about" is a page.
#[tokio::test]
async fn the_about_page_explains_what_this_is_and_its_license() {
    let dir = common::tmp_dir();
    let base = spawn_ui(&dir).await;

    let (status, body) = get(&format!("{base}/about")).await;
    assert_eq!(status, 200);
    assert!(
        body.contains(fedifetcher::VERSION),
        "the page names the version being run"
    );
    assert!(
        body.contains("GNU General Public License"),
        "the page names the license"
    );
    assert!(
        body.contains("https://www.gnu.org/licenses/gpl-3.0.html"),
        "the page links the license text"
    );
    assert!(
        body.contains("WITHOUT ANY WARRANTY"),
        "the page carries the warranty disclaimer"
    );
    assert!(
        body.contains("Michael Thomas") && body.contains("MIT"),
        "the page keeps the original's notice"
    );

    // the top bar's version number links here
    let (status, body) = get(&format!("{base}/")).await;
    assert_eq!(status, 200);
    assert!(
        body.contains("class=\"version\"") && body.contains("href=\"/about\""),
        "the version number links to the about page: {body}"
    );
}

/// The top bar carries the run controls — the status pill, and the start
/// button — on every page, so a run can be driven from anywhere.
#[tokio::test]
async fn the_top_bar_carries_the_run_controls_on_every_page() {
    let dir = common::tmp_dir();
    let base = spawn_ui(&dir).await;

    for page in ["/", "/data", "/logs", "/config", "/debug", "/about"] {
        let (status, body) = get(&format!("{base}{page}")).await;
        assert_eq!(status, 200, "{page}");
        assert!(
            body.contains("class=\"run\""),
            "{page}: the run controls are in the top bar"
        );
        assert!(
            body.contains(">Idle</span>"),
            "{page}: the status pill is in the top bar"
        );
        assert!(
            body.contains("action=\"/run\"") && body.contains(">Start</button>"),
            "{page}: the start button is in the top bar"
        );
    }
}

/// The run-control buttons redirect back to the page they were pressed
/// on, so pressing start or stop is answered by the page itself; with no
/// page to go back to, they land on the dashboard.
#[tokio::test]
async fn run_control_posts_redirect_back_to_the_page_they_were_pressed_on() {
    let dir = common::tmp_dir();
    // a server that is not there, so the run a /run starts fails fast
    let mut config = common::test_config(&dir, "127.0.0.1:9");
    config.access_tokens = vec![TOKEN.to_string()];
    let (base, _) = spawn_ui_with(config).await;

    for action in ["run", "stop", "pause", "resume"] {
        let resp = client()
            .post(format!("{base}/{action}"))
            .header("Referer", format!("{base}/logs?sort=level&dir=asc"))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status().as_u16(), 303, "/{action}");
        assert_eq!(
            resp.headers().get("Location").unwrap(),
            "/logs?sort=level&dir=asc",
            "/{action} goes back to the page it was pressed on"
        );
    }

    // with no page to go back to, they land on the dashboard
    for action in ["stop", "pause", "resume"] {
        let resp = client()
            .post(format!("{base}/{action}"))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status().as_u16(), 303, "/{action}");
        assert_eq!(resp.headers().get("Location").unwrap(), "/", "/{action}");
    }
}

/// The dashboard's State section counts the database's tables as well as
/// what the state itself remembers, so the numbers answer "what has the
/// database recorded" at a glance.
#[tokio::test]
async fn the_dashboard_counts_the_databases_tables() {
    let dir = common::tmp_dir();
    let config = common::test_config(&dir, "example.social");
    let pool = fedifetcher::db::open(&config).await.unwrap();

    let post = |uri: &str| fedifetcher::db::TriggerPost {
        uri: uri.to_string(),
        url: uri.to_string(),
        acct: Some("a@remote.example".to_string()),
        author_url: None,
        replies_count: Some(2),
        created_at: None,
        in_reply_to_id: None,
        in_reply_to_account_id: None,
        is_public: true,
        is_boost: false,
        mentions: None,
        source: "home timeline".to_string(),
    };
    fedifetcher::db::record_trigger_posts(
        &pool,
        &[
            post("https://remote.example/@a/1"),
            post("https://remote.example/@a/2"),
        ],
        1000,
    )
    .await
    .unwrap();
    fedifetcher::db::mark_private_post(&pool, "https://remote.example/@a/1")
        .await
        .unwrap();
    fedifetcher::db::mark_opted_out_account(&pool, "bot@remote.example", "it is not indexable")
        .await
        .unwrap();
    fedifetcher::db::record_known_context_posts(
        &pool,
        &[
            "https://remote.example/@a/3".to_string(),
            "https://remote.example/@a/4".to_string(),
            "https://remote.example/@a/5".to_string(),
        ],
    )
    .await
    .unwrap();
    fedifetcher::db::robots_store(&pool, "https://remote.example/robots.txt", "User-agent: *")
        .await
        .unwrap();
    let log = |message: &str| fedifetcher::db::LogRow {
        ts: "2026-10-22 16:05:07 -05:00".to_string(),
        level: "INFO".to_string(),
        target: "fedifetcher::test".to_string(),
        message: message.to_string(),
    };
    fedifetcher::db::insert_log(&pool, &log("first line"))
        .await
        .unwrap();
    fedifetcher::db::insert_log(&pool, &log("second line"))
        .await
        .unwrap();
    let started = fedifetcher::timeutil::fmt_state_ts(fedifetcher::timeutil::now());
    let id = fedifetcher::db::start_run(&pool, &started).await.unwrap();
    fedifetcher::db::finish_run(&pool, id, &started, "ok", "a run")
        .await
        .unwrap();

    let base = spawn_ui(&dir).await;
    let (status, body) = get(&format!("{base}/")).await;
    assert_eq!(status, 200);
    for (label, count) in [
        ("trigger posts", "2"),
        ("private posts", "1"),
        ("opted-out accounts", "1"),
        ("known context posts", "3"),
        ("robots.txt cache", "1"),
        ("log lines", "2"),
        ("runs recorded", "1"),
    ] {
        assert!(
            body.contains(&format!("<td>{label}</td><td>{count}</td>")),
            "the State section counts {label}: {body}"
        );
    }
}

/// POST a urlencoded form to an arbitrary path: (status, Location, body).
async fn post_config_form(base: &str, path: &str, form: &str) -> (u16, Option<String>, String) {
    let resp = client()
        .post(format!("{base}{path}"))
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(form.to_string())
        .send()
        .await
        .unwrap();
    let status = resp.status().as_u16();
    let location = resp
        .headers()
        .get("Location")
        .map(|value| value.to_str().unwrap().to_string());
    let body = resp.text().await.unwrap();
    (status, location, body)
}

/// An account connected through OAuth is listed on the config page: its
/// acct, its avatar, a Disconnect button — and the connect form above the
/// manual token field, which stays.
#[tokio::test]
async fn the_config_page_lists_the_connected_accounts() {
    let dir = common::tmp_dir();
    let mut config = common::test_config(&dir, "example.social");
    config.access_tokens = vec!["oauth-token-aaaa".to_string()];
    let (base, pool) = spawn_ui_with(config).await;
    fedifetcher::db::save_oauth_account(
        &pool,
        "oauth-token-aaaa",
        "example.social",
        "alice",
        "http://example.social/avatar/alice.png",
    )
    .await
    .unwrap();

    let (status, body) = get(&format!("{base}/config")).await;
    assert_eq!(status, 200);
    assert!(body.contains("alice"), "the account's acct renders: {body}");
    assert!(
        body.contains("http://example.social/avatar/alice.png"),
        "the avatar renders"
    );
    assert!(
        body.contains("Disconnect"),
        "the row offers to disconnect: {body}"
    );
    assert!(
        body.contains("action=\"/oauth/connect\""),
        "the connect form is on the page"
    );
    assert!(
        body.contains("Connect your Fediverse account"),
        "the button speaks fediverse, not just Mastodon"
    );
    assert!(
        body.contains("\"account-row\"") || body.contains("li class=\"account\""),
        "the account rows lay out with their buttons"
    );
    assert!(
        body.contains("Hover a field for what it does, its documented default, and the API scopes its task spends."),
        "the hint names what the tooltips carry"
    );
    assert!(
        body.matches("Save configuration").count() == 2,
        "a save button sits above the table as well as below"
    );
    assert!(
        body.contains("token-row") && body.contains("token-remove"),
        "the token rows carry their remove checkbox to the right"
    );
    assert!(
        body.contains("new_access_token"),
        "the manual paste field stays: {body}"
    );
    assert!(
        !body.contains("<script"),
        "the config page stays script-free: {body}"
    );
}

/// A config page arriving back from the dance says what happened.
#[tokio::test]
async fn the_config_page_reports_what_the_oauth_dance_said() {
    let dir = common::tmp_dir();
    let config = common::test_config(&dir, "example.social");
    let (base, _pool) = spawn_ui_with(config).await;

    let (status, body) = get(&format!("{base}/config?oauth=denied")).await;
    assert_eq!(status, 200);
    assert!(
        body.contains("denied"),
        "the denial is spelled out on the page: {body}"
    );
}

/// Disconnecting revokes the token upstream, removes it from the config,
/// and drops the account's row. The instance is a mock, so the revoke is
/// witnessed.
#[tokio::test]
async fn disconnecting_revokes_the_token_and_removes_the_account() {
    let server = MockServer::start_async().await;
    let dir = common::tmp_dir();
    let instance_host = server.base_url().trim_start_matches("http://").to_string();
    let mut config = common::test_config(&dir, &instance_host);
    config.access_tokens = vec!["oauth-token-aaaa".to_string()];
    let pool = fedifetcher::db::open(&config).await.unwrap();
    fedifetcher::db::save_oauth_account(
        &pool,
        "oauth-token-aaaa",
        &instance_host,
        "alice",
        "http://avatar.example/alice.png",
    )
    .await
    .unwrap();
    let revoke = server
        .mock_async(|when, then| {
            when.method(Method::POST)
                .path("/oauth/revoke")
                .body_includes("token=oauth-token-aaaa");
            then.status(200);
        })
        .await;
    // the app registration the revoke needs, answered from the cache
    fedifetcher::db::save_oauth_app(
        &pool,
        &instance_host,
        "the-client-id",
        "the-secret",
        "read:search read:statuses read:accounts",
    )
    .await
    .unwrap();

    let app = fedifetcher::web::app_with_scheme(config, pool.clone(), "http");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });
    let base = format!("http://{addr}");

    let (status, _, _) = post_config_form(
        &base,
        "/oauth/disconnect",
        &format!("acct=alice&server={instance_host}"),
    )
    .await;
    assert_eq!(status, 303);
    assert_eq!(revoke.calls(), 1, "the token is revoked upstream");

    let saved = saved_config(&pool).await;
    assert!(
        !saved
            .access_tokens
            .contains(&"oauth-token-aaaa".to_string()),
        "the token left the config: {:?}",
        saved.access_tokens
    );
    assert!(
        fedifetcher::db::oauth_account(&pool, "oauth-token-aaaa")
            .await
            .unwrap()
            .is_none(),
        "the account's row is gone"
    );
}

/// A token removed through the form's index checkbox drops the account's
/// OAuth row too: the two paths cannot disagree.
#[tokio::test]
async fn removing_a_token_by_index_drops_its_oauth_row() {
    let dir = common::tmp_dir();
    let mut config = common::test_config(&dir, "example.social");
    config.access_tokens = vec!["oauth-token-aaaa".to_string()];
    let (base, pool) = spawn_ui_with(config).await;
    fedifetcher::db::save_oauth_account(
        &pool,
        "oauth-token-aaaa",
        "example.social",
        "alice",
        "http://avatar.example/alice.png",
    )
    .await
    .unwrap();

    let (status, _, _) = post_config(&base, "remove_access_token=0").await;
    assert_eq!(status, 303);

    assert!(
        fedifetcher::db::oauth_account(&pool, "oauth-token-aaaa")
            .await
            .unwrap()
            .is_none(),
        "the account's row went with the token"
    );
}

/// A connected account whose token the instance rejected renders a
/// Reconnect button that starts a fresh authorize round for its server.
#[tokio::test]
async fn a_marked_account_offers_to_reconnect() {
    let dir = common::tmp_dir();
    let mut config = common::test_config(&dir, "example.social");
    config.access_tokens = vec!["dead-token-aaaa".to_string()];
    let (base, pool) = spawn_ui_with(config).await;
    fedifetcher::db::save_oauth_account(
        &pool,
        "dead-token-aaaa",
        "example.social",
        "alice",
        "http://avatar.example/alice.png",
    )
    .await
    .unwrap();
    fedifetcher::db::mark_oauth_needs_reconnect(&pool, "dead-token-aaaa", true)
        .await
        .unwrap();

    let (status, body) = get(&format!("{base}/config")).await;
    assert_eq!(status, 200);
    assert!(
        body.contains("Reconnect"),
        "the marked row offers to reconnect: {body}"
    );
    assert!(
        !body.contains("Reconnect the marked FediFetcher"),
        "the reconnect is the account's own dance"
    );
}

/// Reconnecting replaces the dead token: the fresh one joins the config,
/// the stale one leaves it, and the mark is cleared.
#[tokio::test]
async fn reconnecting_replaces_the_dead_token() {
    let server = MockServer::start_async().await;
    let dir = common::tmp_dir();
    let instance_host = server.base_url().trim_start_matches("http://").to_string();
    let mut config = common::test_config(&dir, &instance_host);
    config.access_tokens = vec!["dead-token-aaaa".to_string()];
    let pool = fedifetcher::db::open(&config).await.unwrap();
    fedifetcher::db::save_oauth_account(
        &pool,
        "dead-token-aaaa",
        &instance_host,
        "alice",
        "http://avatar.example/alice.png",
    )
    .await
    .unwrap();
    fedifetcher::db::mark_oauth_needs_reconnect(&pool, "dead-token-aaaa", true)
        .await
        .unwrap();

    server
        .mock_async(|when, then| {
            when.method(Method::POST).path("/api/v1/apps");
            then.status(200).json_body(serde_json::json!({
                "id": "17",
                "client_id": "the-client-id",
                "client_secret": "the-client-secret",
            }));
        })
        .await;
    server
        .mock_async(|when, then| {
            when.method(Method::POST).path("/oauth/token");
            then.status(200)
                .json_body(serde_json::json!({ "access_token": "fresh-token-bbbb" }));
        })
        .await;
    server
        .mock_async(|when, then| {
            when.method(Method::GET)
                .path("/api/v1/accounts/verify_credentials");
            then.status(200).json_body(serde_json::json!({
                "acct": "alice",
                "avatar": "http://avatar.example/alice.png",
            }));
        })
        .await;

    let app = fedifetcher::web::app_with_scheme(config, pool.clone(), "http");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });
    let base = format!("http://{addr}");

    // the reconnect dance: connect names the same server, the instance
    // approves, the callback trades the code
    let resp = client()
        .post(format!("{base}/oauth/connect"))
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(format!("server={instance_host}"))
        .send()
        .await
        .unwrap();
    let authorize_url = resp.headers().get("Location").unwrap().to_str().unwrap();
    let authorize = url::Url::parse(authorize_url).unwrap();
    let state = authorize
        .query_pairs()
        .find(|(k, _)| k == "state")
        .map(|(_, v)| v.into_owned())
        .unwrap();
    let cookie = resp
        .headers()
        .get_all("set-cookie")
        .iter()
        .find_map(|value| {
            let text = value.to_str().ok()?;
            text.starts_with("ff_oauth_state=")
                .then(|| text.split(';').next().unwrap_or_default().to_string())
        })
        .unwrap();

    let resp = client()
        .get(format!(
            "{base}/oauth/callback?code=the-dance-code&state={state}"
        ))
        .header("Cookie", &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 303);

    let saved = saved_config(&pool).await;
    assert_eq!(
        saved.access_tokens,
        vec!["fresh-token-bbbb".to_string()],
        "the fresh token replaced the dead one"
    );
    assert!(
        fedifetcher::db::oauth_account(&pool, "dead-token-aaaa")
            .await
            .unwrap()
            .is_none(),
        "the dead token's row is gone"
    );
    let row = fedifetcher::db::oauth_account(&pool, "fresh-token-bbbb")
        .await
        .unwrap()
        .expect("the fresh row is recorded");
    assert!(!row.needs_reconnect, "the fresh token needs no reconnect");
}

/// A connected account whose registration predates a task's scopes says so
/// on its row and offers the reconnect that fixes it.
#[tokio::test]
async fn a_row_missing_a_new_tasks_scopes_warns_and_offers_reconnect() {
    let dir = common::tmp_dir();
    let mut config = common::test_config(&dir, "example.social");
    config.access_tokens = vec!["oauth-token-aaaa".to_string()];
    config.from_lists = true;
    let (base, pool) = spawn_ui_with(config).await;
    fedifetcher::db::save_oauth_account(
        &pool,
        "oauth-token-aaaa",
        "example.social",
        "alice",
        "http://avatar.example/alice.png",
    )
    .await
    .unwrap();
    // the registration the token was granted under: no read:lists, which
    // the now-enabled lists task needs
    fedifetcher::db::save_oauth_app(
        &pool,
        "example.social",
        "the-client-id",
        "the-secret",
        "read:search read:statuses read:accounts",
    )
    .await
    .unwrap();

    let (status, body) = get(&format!("{base}/config")).await;
    assert_eq!(status, 200);
    assert!(
        body.contains("more permissions"),
        "the row says what is missing: {body}"
    );
    assert!(
        body.contains("\"warn-box\""),
        "the warning is boxed: {body}"
    );
    assert!(
        body.contains("Reconnect"),
        "the row offers the reconnect: {body}"
    );
}

/// The mock instance's host, without the scheme.
fn instance_host(server: &MockServer) -> String {
    server.base_url().trim_start_matches("http://").to_string()
}

/// POST a urlencoded form to an arbitrary path, without following
/// redirects: (status, body).
async fn post_form(base: &str, path: &str, form: &str) -> (u16, String) {
    let resp = client()
        .post(format!("{base}{path}"))
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(form.to_string())
        .send()
        .await
        .unwrap();
    let status = resp.status().as_u16();
    let body = resp.text().await.unwrap();
    (status, body)
}

/// Connecting with no server named is a config error, before anything is
/// asked of any instance.
#[tokio::test]
async fn connecting_without_a_server_is_rejected() {
    let dir = common::tmp_dir();
    let (base, _pool) = spawn_ui_with(common::test_config(&dir, "example.social")).await;

    let (status, body) = post_form(&base, "/oauth/connect", "server=").await;
    assert_eq!(status, 400);
    assert!(
        body.contains("the server to connect to is missing"),
        "{body}"
    );
}

/// When the instance refuses the app registration, the connect flow says
/// so instead of redirecting into a doomed dance.
#[tokio::test]
async fn a_failed_registration_is_reported() {
    let server = MockServer::start_async().await;
    server
        .mock_async(|when, then| {
            when.method(Method::POST).path("/api/v1/apps");
            then.status(500);
        })
        .await;
    let dir = common::tmp_dir();
    let config = common::test_config(&dir, &instance_host(&server));
    let db = fedifetcher::db::open(&config).await.unwrap();
    let app = web::app_with_scheme(config, db.clone(), "http");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });
    let base = format!("http://{addr}");

    let mock_host = instance_host(&server);
    let (status, body) = post_form(&base, "/oauth/connect", &format!("server={mock_host}")).await;
    assert_eq!(status, 400);
    assert!(
        body.contains(&format!(
            "registering the app on {mock_host} failed with status 500"
        )),
        "{body}"
    );
}

/// Disconnecting without naming an account and server is a config error.
#[tokio::test]
async fn disconnecting_needs_an_account_and_server() {
    let dir = common::tmp_dir();
    let (base, _pool) = spawn_ui_with(common::test_config(&dir, "example.social")).await;

    let (status, body) = post_form(&base, "/oauth/disconnect", "").await;
    assert_eq!(status, 400);
    assert!(
        body.contains("the account to disconnect is missing"),
        "{body}"
    );
}

/// The connection test against a pasted token (no oauth_accounts row)
/// asks the configured server, not a per-account one.
#[tokio::test]
async fn the_connection_test_falls_back_to_the_configured_server_for_pasted_tokens() {
    let server = MockServer::start_async().await;
    let instance = server
        .mock_async(|when, then| {
            when.method(Method::GET).path("/api/v1/instance");
            then.status(200).json_body(
                serde_json::json!({"title": "Home", "uri": "example.social", "version": "4.2.0"}),
            );
        })
        .await;
    let verify = server
        .mock_async(|when, then| {
            when.method(Method::GET)
                .path("/api/v1/accounts/verify_credentials")
                .header("Authorization", "Bearer smoke-secret-token-123");
            then.status(200)
                .json_body(serde_json::json!({"id": "1", "acct": "me", "display_name": "Me"}));
        })
        .await;
    let dir = common::tmp_dir();
    let mut config = common::test_config(&dir, &instance_host(&server));
    config.access_tokens = vec![TOKEN.to_string()];
    let db = fedifetcher::db::open(&config).await.unwrap();
    let app = web::app_with_scheme(config, db.clone(), "http");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });
    let base = format!("http://{addr}");

    let (status, body) = post_debug(&base, "action=connect&token_index=0").await;
    assert_eq!(status, 200);
    assert!(
        body.contains("Home"),
        "the configured server answered: {body}"
    );
    assert_eq!(instance.calls(), 1);
    assert_eq!(verify.calls(), 1);
}

/// The user lookup and the posts fetch work end-to-end through the web
/// layer: the result views render, long notes and posts are cut to a
/// snippet, and no full content floods the page.
#[tokio::test]
async fn the_debug_user_and_posts_actions_work_through_the_web() {
    let server = MockServer::start_async().await;
    let long_note = "n".repeat(250);
    let long_content = "c".repeat(250);
    let lookup = server
        .mock_async(|when, then| {
            when.method(Method::GET)
                .path("/api/v1/accounts/lookup")
                .query_param("acct", "someone@example.social")
                .header("Authorization", "Bearer smoke-secret-token-123");
            then.status(200).json_body(serde_json::json!(
                {"id": "42", "acct": "someone@example.social", "display_name": "Someone",
                 "note": long_note}
            ));
        })
        .await;
    let statuses = server
        .mock_async(|when, then| {
            when.method(Method::GET)
                .path("/api/v1/accounts/42/statuses")
                .header("Authorization", "Bearer smoke-secret-token-123");
            then.status(200).json_body(serde_json::json!([
                {"url": "https://example.social/@someone/7", "content": long_content,
                 "visibility": "public", "created_at": "2026-10-24T12:00:00.000Z",
                 "replies_count": 1, "reblogs_count": 2, "favourites_count": 3}
            ]));
        })
        .await;
    let dir = common::tmp_dir();
    let mut config = common::test_config(&dir, &instance_host(&server));
    config.access_tokens = vec![TOKEN.to_string()];
    let db = fedifetcher::db::open(&config).await.unwrap();
    let app = web::app_with_scheme(config, db.clone(), "http");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });
    let base = format!("http://{addr}");

    let (status, body) = post_debug(&base, "action=user&user=someone@example.social").await;
    assert_eq!(status, 200);
    assert!(body.contains("Found"), "the lookup result renders: {body}");
    assert_eq!(lookup.calls(), 1);
    assert!(
        body.contains(&"n".repeat(200)),
        "the note is kept up to the snippet length"
    );

    let (status, body) =
        post_debug(&base, "action=posts&user=someone@example.social&limit=2").await;
    assert_eq!(status, 200);
    assert!(
        body.contains("Recent posts"),
        "the posts result renders: {body}"
    );
    assert_eq!(statuses.calls(), 1);
    assert!(
        body.contains("..."),
        "the long content is cut to a snippet: {body}"
    );
    assert!(
        !body.contains(&"c".repeat(250)),
        "the full content must not flood the page"
    );
}

/// The config page spells out the quiet dance endings too: an expired
/// state and a callback with no code.
#[tokio::test]
async fn the_config_page_reports_expired_and_failed_dances() {
    let dir = common::tmp_dir();
    let config = common::test_config(&dir, "example.social");
    let (base, _pool) = spawn_ui_with(config).await;

    let (status, body) = get(&format!("{base}/config?oauth=expired")).await;
    assert_eq!(status, 200);
    assert!(
        body.contains("not started here or has expired"),
        "the expired message: {body}"
    );

    let (status, body) = get(&format!("{base}/config?oauth=failed")).await;
    assert_eq!(status, 200);
    assert!(
        body.contains("did not return an authorization code"),
        "the failed message: {body}"
    );
}

/// A connected account whose registration covers everything the enabled
/// tasks ask for is listed with no warning at all.
#[tokio::test]
async fn a_fully_scoped_account_shows_no_warning() {
    let dir = common::tmp_dir();
    let mut config = common::test_config(&dir, "example.social");
    config.access_tokens = vec!["oauth-token-aaaa".to_string()];
    let (base, pool) = spawn_ui_with(config).await;
    fedifetcher::db::save_oauth_app(
        &pool,
        "example.social",
        "the-client-id",
        "the-client-secret",
        "read:search read:statuses read:accounts",
    )
    .await
    .unwrap();
    fedifetcher::db::save_oauth_account(&pool, "oauth-token-aaaa", "example.social", "alice", "")
        .await
        .unwrap();

    let (status, body) = get(&format!("{base}/config")).await;
    assert_eq!(status, 200);
    assert!(body.contains("alice"), "the account is listed: {body}");
    assert!(
        !body.contains("more permissions") && !body.contains("rejected this token"),
        "a fully scoped row must not warn: {body}"
    );
}

/// An empty field for an optional option saves as unset, and unknown form
/// keys are ignored rather than rejected.
#[tokio::test]
async fn an_empty_optional_option_saves_as_unset() {
    let dir = common::tmp_dir();
    let (base, pool) = spawn_ui_with(common::test_config(&dir, "example.social")).await;

    let (status, location, _body) =
        post_config_form(&base, "/config", "lock_file=&junk=ignored").await;
    assert_eq!(status, 303);
    assert!(location.unwrap().starts_with("/config"));
    let saved = saved_config(&pool).await;
    assert!(
        saved.lock_file.is_none(),
        "the empty optional option is unset: {:?}",
        saved.lock_file
    );
}

/// A database that is gone (closed after startup) still renders every page —
/// with the failures named in the log, not silently empty tables.
#[tokio::test]
async fn a_data_page_whose_database_is_gone_warns_and_renders() {
    let dir = common::tmp_dir();
    let mut config = common::test_config(&dir, "example.social");
    config.access_tokens = vec![TOKEN.to_string()];
    let db = fedifetcher::db::open(&config).await.unwrap();
    db.close().await;

    // the page-load capture is this binary's one global subscriber; using it
    // here too (its install is idempotent) instead of a second installer
    install_page_load_capture();
    let app = web::app(config, db.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });
    let base = format!("http://{addr}");

    let (status, body) = get(&format!("{base}/data")).await;

    assert_eq!(status, 200, "the page renders regardless: {body}");
    assert!(
        page_load_lines()
            .iter()
            .any(|line| line.contains("Could not read the")),
        "the database's failure is named: {:?}",
        page_load_lines()
    );
}

/// The debug page reports the database's size, and row counts only for the
/// five tables that take the most space in it.
#[tokio::test]
async fn the_debug_page_reports_the_database_size_and_usage() {
    let dir = common::tmp_dir();
    let mut config = common::test_config(&dir, "example.social");
    config.access_tokens = vec![TOKEN.to_string()];
    let (base, db) = spawn_ui_with(config).await;
    // give the logs table enough rows to span pages, as it would in a
    // real deployment: an empty table still occupies its root page, so a
    // handful of rows would tie with every empty table at one page
    for i in 0..300 {
        fedifetcher::db::insert_log(
            &db,
            &fedifetcher::db::LogRow {
                ts: "2026-01-01 00:00:00+00:00".to_string(),
                level: "INFO".to_string(),
                message: format!("filler {i}: {}", "x".repeat(200)),
                target: "test".to_string(),
            },
        )
        .await
        .unwrap();
    }
    // the space reading comes from the main database file, so the
    // write-ahead log's rows are checkpointed into it first
    sqlx::query("PRAGMA wal_checkpoint(TRUNCATE)")
        .execute(&db)
        .await
        .unwrap();

    let (status, page) = get(&format!("{base}/debug")).await;

    assert_eq!(status, 200);
    assert!(
        page.contains("Database"),
        "the page has a database section: {page}"
    );
    assert!(
        page.contains("fedifetcher.db"),
        "the database file is named: {page}"
    );
    assert!(page.contains("B"), "a size is reported: {page}");
    assert!(
        page.contains("biggest:"),
        "the counts are introduced as the biggest tables: {page}"
    );
    assert!(
        page.contains("log lines"),
        "the biggest table's count is reported: {page}"
    );
    // a table that is not among the biggest five is not counted
    assert!(
        !page.contains("runs recorded"),
        "small tables go uncounted: {page}"
    );
}

/// The Clean up DB button removes what the retention rules would remove,
/// and the page reports what went.
#[tokio::test]
async fn the_cleanup_button_cleans_and_reports() {
    let dir = common::tmp_dir();
    let mut config = common::test_config(&dir, "example.social");
    config.access_tokens = vec![TOKEN.to_string()];
    let (base, db) = spawn_ui_with(config).await;
    sqlx::query("INSERT INTO config (key, value) VALUES ('server', 'example.social')")
        .execute(&db)
        .await
        .unwrap();
    for i in 0..5 {
        fedifetcher::db::insert_log(
            &db,
            &fedifetcher::db::LogRow {
                ts: "2026-01-01 00:00:00+00:00".to_string(),
                level: "INFO".to_string(),
                target: "t".to_string(),
                message: format!("line {i}"),
            },
        )
        .await
        .unwrap();
    }

    let (status, page) = post_debug(base.as_str(), "action=cleanup").await;

    assert_eq!(status, 200);
    assert!(
        page.contains("Cleaned up the database"),
        "the cleanup is reported: {page}"
    );
    assert!(
        page.contains("log lines"),
        "the report counts what each emptied table held: {page}"
    );
    let (status, _) = get(&format!("{base}/debug")).await;
    assert_eq!(status, 200);
    assert_eq!(
        fedifetcher::db::table_count(&db, "logs").await.unwrap(),
        0,
        "the logs were emptied"
    );
    // the config page's own records survive a cleanup
    assert_eq!(
        fedifetcher::db::table_count(&db, "config").await.unwrap(),
        1,
        "the config records stay"
    );
    let (status, page) = get(&format!("{base}/debug")).await;
    assert_eq!(status, 200);
    assert!(
        page.contains("Clean up DB"),
        "the page offers the button again: {page}"
    );
}

/// The Trim 10% button sits next to Clean up DB, and removes only the
/// oldest tenth of each cleanable table.
#[tokio::test]
async fn the_trim_button_trims_the_oldest_tenth_and_reports() {
    let dir = common::tmp_dir();
    let mut config = common::test_config(&dir, "example.social");
    config.access_tokens = vec![TOKEN.to_string()];
    let (base, db) = spawn_ui_with(config).await;
    for i in 0..10 {
        fedifetcher::db::insert_log(
            &db,
            &fedifetcher::db::LogRow {
                ts: fedifetcher::timeutil::fmt_state_ts(
                    fedifetcher::timeutil::now() - time::Duration::minutes(100 - i),
                ),
                level: "INFO".to_string(),
                target: "t".to_string(),
                message: format!("line {i}"),
            },
        )
        .await
        .unwrap();
    }

    let (_, page) = get(&format!("{base}/debug")).await;
    assert!(
        page.contains("Trim 10%"),
        "the page offers the trim next to the clear: {page}"
    );

    let (status, page) = post_debug(base.as_str(), "action=trim").await;

    assert_eq!(status, 200);
    assert!(
        page.contains("Trimmed the oldest 10%"),
        "the trim is reported: {page}"
    );
    assert_eq!(
        fedifetcher::db::table_count(&db, "logs").await.unwrap(),
        9,
        "one of the ten log lines went"
    );
}

/// Saving the config applies the log level and format right away: the page
/// no longer says a restart is needed.
#[tokio::test]
async fn the_log_settings_apply_without_a_restart() {
    let dir = common::tmp_dir();
    let config = common::test_config(&dir, "example.social");
    let (base, _db) = spawn_ui_with(config).await;

    let (status, page) = get(&format!("{base}/config")).await;
    assert_eq!(status, 200);
    assert!(
        !page.contains("apply on restart"),
        "no restart needed any more: {page}"
    );

    // saving a new level and format succeeds, and the running logger takes
    // them (the logging module's own test pins the logger side)
    let (status, _, _) = post_config(
        base.as_str(),
        "server=example.social&access_tokens=smoke-secret-token-123&log_level=WARNING&log_format=%25%28levelname%29s%3A+%25%28message%29s",
    )
    .await;
    assert_eq!(status, 303);
}

/// The instructional sentences the pages used to carry are gone; what is
/// left says only what the thing is.
#[tokio::test]
async fn the_how_to_use_the_page_sentences_are_gone() {
    let dir = common::tmp_dir();
    seed_hosts(
        &dir,
        serde_json::json!({
            "alpha.example": host_entry("Mastodon", "4.2.0", &["mastodon"], 3),
        }),
    )
    .await;
    seed_db(&dir).await;
    let base = spawn_ui(&dir).await;

    for path in ["/config", "/data", "/logs", "/data?tab=runs"] {
        let (status, page) = get(&format!("{base}{path}")).await;
        assert_eq!(status, 200, "{path}");
        for gone in [
            "no token to copy by hand",
            "Click a column heading to sort by it",
            "The last column fetches that one thing now",
            "The page refreshes itself every",
        ] {
            assert!(
                !page.contains(gone),
                "{path} no longer says {gone:?}: {page}"
            );
        }
    }
    let (_, page) = get(&format!("{base}/logs")).await;
    assert!(
        page.contains("entries from the database"),
        "the logs page still says what it shows: {page}"
    );
}

/// The remove-token checkbox sits inside its row, not past its edge.
#[tokio::test]
async fn the_remove_token_checkbox_stays_inside_its_row() {
    let dir = common::tmp_dir();
    let config = common::test_config(&dir, "example.social");
    let (base, _db) = spawn_ui_with(config).await;

    let (status, page) = get(&format!("{base}/config")).await;
    assert_eq!(status, 200);
    assert!(
        page.contains("label.token-remove { margin-left: auto; margin-right: 0.25rem;"),
        "the checkbox is pulled inside the row's edge: {page}"
    );
}

/// A cell holding a JSON document renders pretty-printed, so the hosts
/// table's raw_nodeinfo is readable instead of a dense one-liner.
#[tokio::test]
async fn a_json_cell_renders_pretty_printed() {
    let dir = common::tmp_dir();
    let config = common::test_config(&dir, "example.social");
    let pool = fedifetcher::db::open(&config).await.unwrap();
    sqlx::query(
        "INSERT INTO seen_hosts (host, webserver, software, version, mastodon_api,
         misskey_api, lemmy_api, peertube_api, last_checked, error, raw_nodeinfo)
         VALUES ('pretty.example', 'pretty.example', 'mastodon', '4.2.0', 1, 0, 0, 0,
         '2026-10-22 16:05:07+00:00', NULL,
         '{\"software\":{\"name\":\"mastodon\",\"version\":\"4.2.0\"},\"protocols\":[\"activitypub\"]}')",
    )
    .execute(&pool)
    .await
    .unwrap();
    let app = web::app(config, pool);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.ok() });
    let base = format!("http://{addr}");

    let (status, page) = get(&format!("{base}/data?tab=seen_hosts")).await;

    assert_eq!(status, 200);
    assert!(
        page.contains("<pre class=\"json\">"),
        "the JSON cell is a pre block: {page}"
    );
    // pretty-printed: a space after each colon, one key per line
    assert!(
        page.contains("\"name\": \"mastodon\""),
        "the document is pretty-printed: {page}"
    );
    assert!(page.contains('\n'), "the document spans lines");
    // plain cells are not wrapped in pre
    assert!(
        !page.contains("<pre class=\"json\">mastodon<"),
        "the software column stays a plain cell: {page}"
    );
}

/// The dashboard keeps only the State section and the run history: the
/// run controls live in the top bar on every page, and the enabled tasks
/// and counts are on the Config and Data pages.
#[tokio::test]
async fn the_dashboard_keeps_only_state_and_recent_runs() {
    let dir = common::tmp_dir();
    let base = spawn_ui(&dir).await;

    let (status, page) = get(&format!("{base}/")).await;
    assert_eq!(status, 200);
    assert!(page.contains("State"), "the State section stays: {page}");
    assert!(
        page.contains("Recent runs"),
        "the run history stays: {page}"
    );
    for gone in [
        "FediFetch NOW",
        "Enabled tasks",
        "Status:",
        "access token(s)",
    ] {
        assert!(
            !page.contains(gone),
            "the dashboard no longer says {gone:?}: {page}"
        );
    }
    // the run controls are still reachable, from the top bar
    assert!(page.contains(">Start</button>"));
}

/// URLs the tables show are clickable: the URL-bearing columns of the
/// named tables link out, and the hosts tab's host names link to the
/// server's own https address.
#[tokio::test]
async fn the_tables_urls_are_clickable() {
    let dir = common::tmp_dir();
    let config = common::test_config(&dir, "example.social");
    let pool = fedifetcher::db::open(&config).await.unwrap();
    let url = "https://remote.example/@alice/1";
    sqlx::query("INSERT INTO seen_urls (uri, first_seen) VALUES (?1, '2026-01-01')")
        .bind(url)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO context_cache (uri, last_seen, created_at) VALUES (?1, '2026-01-01', '2026-01-01')")
        .bind(url)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO known_context_posts (uri, first_seen, last_seen) VALUES (?1, '2026-01-01', '2026-01-01')")
        .bind(url)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO private_posts (uri, first_marked) VALUES (?1, '2026-01-01')")
        .bind(url)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO robots_cache (url, text, fetched_at) VALUES (?1, '', '2026-01-01')")
        .bind(url)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO trigger_posts (uri, url, source, first_seen, last_seen) VALUES (?1, ?1, 'test', '2026-01-01', '2026-01-01')")
        .bind(url)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO seen_hosts (host, webserver, software, version, mastodon_api,
         misskey_api, lemmy_api, peertube_api, last_checked, error, raw_nodeinfo)
         VALUES ('host.example', 'host.example', 'mastodon', '4', 1, 0, 0, 0,
         ?1, NULL, '{}')",
    )
    .bind(fedifetcher::timeutil::fmt_state_ts(
        fedifetcher::timeutil::now(),
    ))
    .execute(&pool)
    .await
    .unwrap();
    let app = web::app(config, pool);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.ok() });
    let base = format!("http://{addr}");

    for tab in [
        "seen_urls",
        "context_cache",
        "known_context_posts",
        "private_posts",
        "robots_cache",
        "trigger_posts",
    ] {
        let (status, page) = get(&format!("{base}/data?tab={tab}")).await;
        assert_eq!(status, 200, "{tab}");
        assert!(
            page.contains(&format!("href=\"{url}\"")),
            "the {tab} table's URL is a link: {page}"
        );
    }

    // the hosts tab's host name links to the server itself
    let (status, page) = get(&format!("{base}/data?tab=hosts")).await;
    assert_eq!(status, 200);
    assert!(
        page.contains("href=\"https://host.example\""),
        "the host name links to the server: {page}"
    );
}

/// The data page follows the live-updates toggle alone — the master
/// control — and never refreshes on its own: a run filling its tables is
/// not worth losing your place in a long list.
#[tokio::test]
async fn the_data_page_follows_the_toggle_alone() {
    let dir = common::tmp_dir();
    let config = common::test_config(&dir, "example.social");
    let (base, db) = spawn_ui_with(config).await;
    let started = fedifetcher::timeutil::fmt_state_ts(fedifetcher::timeutil::now());
    fedifetcher::db::start_run(&db, &started).await.unwrap();

    // a run in progress, live off: the page holds still
    let (status, body) = get_with_cookie(&format!("{base}/data"), "ff_live=0").await;
    assert_eq!(status, 200);
    assert!(
        !body.contains("http-equiv=\"refresh\""),
        "the data page does not refresh on its own: {body}"
    );

    // live on: the page follows the toggle, idle or not
    let (status, body) = get_with_cookie(&format!("{base}/data"), "ff_live=1").await;
    assert_eq!(status, 200);
    assert!(
        body.contains("http-equiv=\"refresh\""),
        "live on, the data page refreshes: {body}"
    );
}

/// The logs page follows a run in progress the same way: new lines are
/// written the whole time, so the page refreshes even with live updates
/// toggled off.
#[tokio::test]
async fn the_logs_page_refreshes_while_a_run_is_in_progress() {
    let dir = common::tmp_dir();
    let config = common::test_config(&dir, "example.social");
    let (base, db) = spawn_ui_with(config).await;
    let started = fedifetcher::timeutil::fmt_state_ts(fedifetcher::timeutil::now());
    fedifetcher::db::start_run(&db, &started).await.unwrap();

    let (status, body) = get_with_cookie(&format!("{base}/logs"), "ff_live=0").await;
    assert_eq!(status, 200);
    assert!(
        body.contains("http-equiv=\"refresh\""),
        "a run in progress refreshes the logs page even with live off: {body}"
    );

    // idle again with live off, the page holds still
    fedifetcher::db::finish_run(&db, 1, &started, "ok", "done")
        .await
        .unwrap();
    let (status, body) = get_with_cookie(&format!("{base}/logs"), "ff_live=0").await;
    assert_eq!(status, 200);
    assert!(
        !body.contains("http-equiv=\"refresh\""),
        "idle, the logs page does not refresh: {body}"
    );
}
/// The debug page follows a run in progress the way the dashboard does:
/// the database usage it shows changes while a run writes to the database.
#[tokio::test]
async fn the_debug_page_refreshes_while_a_run_is_in_progress() {
    let dir = common::tmp_dir();
    let config = common::test_config(&dir, "example.social");
    let (base, db) = spawn_ui_with(config).await;
    let started = fedifetcher::timeutil::fmt_state_ts(fedifetcher::timeutil::now());
    fedifetcher::db::start_run(&db, &started).await.unwrap();

    let (status, body) = get_with_cookie(&format!("{base}/debug"), "ff_live=0").await;
    assert_eq!(status, 200);
    assert!(
        body.contains("http-equiv=\"refresh\""),
        "a run in progress refreshes the debug page even with live off: {body}"
    );

    // idle again, the page holds still
    fedifetcher::db::finish_run(&db, 1, &started, "ok", "done")
        .await
        .unwrap();
    let (status, body) = get_with_cookie(&format!("{base}/debug"), "ff_live=0").await;
    assert_eq!(status, 200);
    assert!(
        !body.contains("http-equiv=\"refresh\""),
        "idle, the debug page does not refresh: {body}"
    );
}

/// The live-updates toggle is the one control for auto-refresh: on, every
/// page that can refresh does — idle or not; off, none does, except the
/// pages that follow a run in progress. The config and about pages never
/// refresh: one holds a form half-filled, the other nothing live.
#[tokio::test]
async fn the_live_toggle_is_the_master_control_for_auto_refresh() {
    let dir = common::tmp_dir();
    let base = spawn_ui(&dir).await;

    for page in ["/", "/data", "/logs", "/debug"] {
        let (status, body) = get_with_cookie(&format!("{base}{page}"), "ff_live=1").await;
        assert_eq!(status, 200, "{page}");
        assert!(
            body.contains("http-equiv=\"refresh\""),
            "live on, {page} refreshes even while idle: {body}"
        );
    }
    for page in ["/", "/data", "/logs", "/debug"] {
        let (status, body) = get_with_cookie(&format!("{base}{page}"), "ff_live=0").await;
        assert_eq!(status, 200, "{page}");
        assert!(
            !body.contains("http-equiv=\"refresh\""),
            "live off and idle, {page} holds still: {body}"
        );
    }
    for page in ["/config", "/about"] {
        let (status, body) = get_with_cookie(&format!("{base}{page}"), "ff_live=1").await;
        assert_eq!(status, 200, "{page}");
        assert!(
            !body.contains("http-equiv=\"refresh\""),
            "{page} never refreshes itself: {body}"
        );
    }
}

/// The top bar's gradient uses the page's own greys: a soft dark-to-medium
/// ramp that suits the light and dark schemes alike.
#[tokio::test]
async fn the_top_bar_is_a_grey_gradient() {
    let dir = common::tmp_dir();
    let base = spawn_ui(&dir).await;

    let (status, page) = get(&format!("{base}/")).await;
    assert_eq!(status, 200);
    assert!(
        page.contains("header {") && page.contains("linear-gradient(to right, #3a3a3a, #757575)"),
        "the header carries the grey gradient: {page}"
    );
    assert!(
        page.contains("header h1 a { color: #eee"),
        "the header's text is light on the dark bar: {page}"
    );
}

/// Hovering an option's name shows the same tooltip as its field: the
/// description rides on the name's cell too.
#[tokio::test]
async fn the_config_names_carry_the_same_tooltip_as_their_fields() {
    let dir = common::tmp_dir();
    let config = common::test_config(&dir, "example.social");
    let (base, _db) = spawn_ui_with(config).await;

    let (status, page) = get(&format!("{base}/config")).await;
    assert_eq!(status, 200);
    let description =
        fedifetcher::config::option_description("http_timeout").expect("the option is documented");
    let prefix: String = description.chars().take(24).collect();
    assert!(
        page.contains(&format!("<td title=\"{prefix}")),
        "the name cell carries the field's tooltip: {page}"
    );
}
