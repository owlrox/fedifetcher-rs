//! The web UI over real HTTP: every page renders, the run trigger answers
//! with a redirect, and there is no JavaScript anywhere.

mod common;

use fedifetcher::api::mastodon::masked;
use fedifetcher::web;
use httpmock::prelude::*;
use httpmock::Method;

const TOKEN: &str = "smoke-secret-token-123";

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
    fedifetcher::config::Config::from_values(raw).unwrap()
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
        ("/", "FediFetch NOW!"),
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
        body.contains("title=\"Required: tokens generated at"),
        "the access tokens field has a tooltip"
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
    let mut body = String::new();
    for _ in 0..50 {
        let (_, page) = get(&format!("{base}/")).await;
        if page.contains("Processing finished in") {
            body = page;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
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
        body.contains("running for"),
        "its end time cell shows how long it has run"
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

    // no runs yet: idle, in the top bar and in the Run section
    let (status, body) = get(&format!("{base}/")).await;
    assert_eq!(status, 200);
    assert!(
        body.contains("<span class=\"badge ok\">Idle</span>"),
        "idle, in the top bar and the Run section: {body}"
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
    let (status, body) = get(&format!("{base}/")).await;
    assert_eq!(status, 200);
    assert!(
        body.contains(&format!(
            "<span class=\"badge running\">Running since {started}</span>"
        )),
        "the Run section follows the history row: {body}"
    );

    // the row finishes: the pill and the Run section go back to idle,
    // from the same read
    fedifetcher::db::finish_run(&db, id, &started, "ok", "done")
        .await
        .unwrap();
    let (status, body) = get(&format!("{base}/")).await;
    assert_eq!(status, 200);
    assert!(
        body.contains("<span class=\"badge ok\">Idle</span>"),
        "the pill and the Run section are idle again: {body}"
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
    for gone in ["logs", "config"] {
        assert!(
            !body.contains(&format!("href=\"/data?tab={gone}\"")),
            "the {gone} table has its own page, not a sub-tab"
        );
    }
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

    // idle: the dashboard offers to run, not to stop
    let (status, body) = get(&format!("{base}/")).await;
    assert_eq!(status, 200);
    assert!(body.contains("FediFetch NOW!"));
    assert!(!body.contains("Stop the running FediFetcher"));
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

    // while the run is going, the dashboard offers to stop it
    let mut running_page = String::new();
    for _ in 0..100 {
        let (_, page) = get(&format!("{base}/")).await;
        if page.contains("Running since") {
            running_page = page;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    assert!(
        running_page.contains("Running since"),
        "the run should be running"
    );
    assert!(running_page.contains("Stop the running FediFetcher"));
    assert!(!running_page.contains("FediFetch NOW!"));

    let resp = client().post(format!("{base}/stop")).send().await.unwrap();
    assert_eq!(resp.status().as_u16(), 303);
    assert_eq!(resp.headers().get("Location").unwrap(), "/");

    // the run finishes early, and the history records that it was stopped,
    // along with how long it ran before stopping
    let mut body = String::new();
    for _ in 0..100 {
        let (_, page) = get(&format!("{base}/")).await;
        if page.contains("Stopped after ") {
            body = page;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
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

    // while the run is going, the dashboard offers to pause it
    let mut running_page = String::new();
    for _ in 0..100 {
        let (_, page) = get(&format!("{base}/")).await;
        if page.contains("Running since") {
            running_page = page;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    assert!(
        running_page.contains("Running since"),
        "the run should be running"
    );
    assert!(running_page.contains("Pause the running FediFetcher"));
    assert!(running_page.contains("Stop the running FediFetcher"));
    assert!(!running_page.contains("Resume the paused FediFetcher"));

    let resp = client().post(format!("{base}/pause")).send().await.unwrap();
    assert_eq!(resp.status().as_u16(), 303);
    assert_eq!(resp.headers().get("Location").unwrap(), "/");

    // the dashboard shows the pause: the badge, and a resume button in
    // place of the pause one — stop stays available
    let mut paused_page = String::new();
    for _ in 0..100 {
        let (_, page) = get(&format!("{base}/")).await;
        if page.contains("badge paused") {
            paused_page = page;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    assert!(
        paused_page.contains("badge paused"),
        "the status badge should say paused"
    );
    assert!(paused_page.contains("Paused (running since"));
    assert!(paused_page.contains("Resume the paused FediFetcher"));
    assert!(paused_page.contains("Stop the running FediFetcher"));
    assert!(!paused_page.contains("Pause the running FediFetcher"));

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
    let mut resumed_page = String::new();
    for _ in 0..100 {
        let (_, page) = get(&format!("{base}/")).await;
        if page.contains("Running since") || page.contains("Processing finished in") {
            resumed_page = page;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    assert!(
        !resumed_page.is_empty(),
        "the resume should set the run going again"
    );
    assert!(!resumed_page.contains("Resume the paused FediFetcher"));

    // and the run finishes once it is going again
    let mut body = String::new();
    for _ in 0..100 {
        let (_, page) = get(&format!("{base}/")).await;
        if page.contains("Processing finished in") {
            body = page;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
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
        "server=example.social&new_access_token=smoke-secret-token-123&max_followers=5&from_lists=1&respect_robots_txt=0&http_timeout=9&hide_server_in_user_agent=1&log_page_loads=1",
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
