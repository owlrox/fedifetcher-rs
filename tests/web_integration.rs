//! The web UI over real HTTP: every page renders, the run trigger answers
//! with a redirect, and there is no JavaScript anywhere.

mod common;

use fedifetcher::api::mastodon::masked;
use fedifetcher::web;
use httpmock::Method;
use httpmock::prelude::*;

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
    fedifetcher::config::build_config(raw).unwrap()
}

#[tokio::test]
async fn every_page_renders_without_javascript() {
    let dir = common::tmp_dir();
    let base = spawn_ui(&dir).await;

    for (path, marker) in [
        ("/", "FediFetch NOW!"),
        ("/hosts", "Seen hosts"),
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
        assert!(!body.contains("<script"), "{path} must not contain JavaScript");
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
    assert!(body.contains("respect_robots_txt"), "the new options are shown");
    assert!(body.contains("respect_user_opt_out"), "the new options are shown");
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
    assert!(
        body.contains("<table"),
        "the log should render as a table"
    );
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
    insert_log(&dir, "2026-10-22 16:05:03 -05:00", "WARNING", "a warning line").await;
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
    assert!(body.contains("(asc)"), "the sorted column shows its direction");
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

    insert_log(&dir, "2026-10-22 16:05:07 -05:00", "INFO", "a line to clear").await;

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
        state.seen_hosts.add(host, info.clone());
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
    let (status, body) = get(&format!("{base}/hosts")).await;
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
        body.contains("/hosts?tab=hosts&amp;sort=software&amp;dir=asc"),
        "a new column sorts the page's default way"
    );

    // by software, ascending: Akkoma, Mastodon, Pleroma
    let (_, body) = get(&format!("{base}/hosts?sort=software&dir=asc")).await;
    assert!(
        position(&body, "beta.example") < position(&body, "alpha.example")
            && position(&body, "alpha.example") < position(&body, "gamma.example"),
        "software ascending is Akkoma, Mastodon, Pleroma"
    );
    assert!(body.contains("(asc)"), "the sorted column shows its direction");
    assert!(
        body.contains("/hosts?tab=hosts&amp;sort=software&amp;dir=desc"),
        "clicking the sorted column flips the direction"
    );

    // by software, descending: Pleroma, Mastodon, Akkoma
    let (_, body) = get(&format!("{base}/hosts?sort=software&dir=desc")).await;
    assert!(
        position(&body, "gamma.example") < position(&body, "alpha.example")
            && position(&body, "alpha.example") < position(&body, "beta.example"),
        "software descending is Pleroma, Mastodon, Akkoma"
    );

    // by version, ascending: 3.13.0, 4.1.0, 4.2.0
    let (_, body) = get(&format!("{base}/hosts?sort=version&dir=asc")).await;
    assert!(
        position(&body, "beta.example") < position(&body, "gamma.example")
            && position(&body, "gamma.example") < position(&body, "alpha.example"),
        "version ascending is 3.13.0, 4.1.0, 4.2.0"
    );

    // by last checked, descending: 1, 2, 3 days ago
    let (_, body) = get(&format!("{base}/hosts?sort=last_checked&dir=desc")).await;
    assert!(
        position(&body, "beta.example") < position(&body, "gamma.example")
            && position(&body, "gamma.example") < position(&body, "alpha.example"),
        "the most recently checked host comes first"
    );

    // an unknown sort falls back to hosts alphabetically
    let (_, body) = get(&format!("{base}/hosts?sort=nonsense")).await;
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
    let run = |started: &str, finished: &str, outcome: &str, message: &str| fedifetcher::db::RunRow {
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
    let (status, body) = get(&format!("{base}/hosts")).await;
    assert_eq!(status, 200);
    for tab in ["hosts", "opted_out_accounts", "private_posts", "runs", "seen_urls"] {
        assert!(
            body.contains(&format!("href=\"/hosts?tab={tab}\"")),
            "the {tab} sub-tab is linked"
        );
    }
    for gone in ["logs", "config"] {
        assert!(
            !body.contains(&format!("href=\"/hosts?tab={gone}\"")),
            "the {gone} table has its own page, not a sub-tab"
        );
    }
    assert!(body.contains("(2)"), "a sub-tab shows its row count");
    // the hosts tab is the default
    assert!(body.contains("Seen hosts"));
    assert!(body.contains("alpha.example"));

    // a table tab shows the table's rows, newest first by default
    let (status, body) = get(&format!("{base}/hosts?tab=runs")).await;
    assert_eq!(status, 200);
    assert!(body.contains("first run"));
    assert!(body.contains("second run"));
    assert!(
        position(&body, "second run") < position(&body, "first run"),
        "the newest row comes first by default"
    );
    assert!(
        body.contains("/hosts?tab=runs&amp;sort=outcome&amp;dir=asc"),
        "a column heading links to that sort"
    );

    // sorted by a column, ascending: error before ok
    let (_, body) = get(&format!("{base}/hosts?tab=runs&sort=outcome&dir=asc")).await;
    assert!(
        position(&body, "first run") < position(&body, "second run"),
        "outcome ascending is error, ok"
    );
    // and descending flips it
    let (_, body) = get(&format!("{base}/hosts?tab=runs&sort=outcome&dir=desc")).await;
    assert!(
        position(&body, "second run") < position(&body, "first run"),
        "outcome descending is ok, error"
    );
    // an unknown column falls back to the default sort, newest first
    let (_, body) = get(&format!("{base}/hosts?tab=runs&sort=nonsense")).await;
    assert!(
        position(&body, "second run") < position(&body, "first run"),
        "an unknown column falls back to the newest first"
    );

    // the other tables show their rows too
    let (status, body) = get(&format!("{base}/hosts?tab=private_posts")).await;
    assert_eq!(status, 200);
    assert!(body.contains("https://remote.example/@a/1"));
    let (status, body) = get(&format!("{base}/hosts?tab=opted_out_accounts")).await;
    assert_eq!(status, 200);
    assert!(body.contains("bot@remote.example"));
    assert!(body.contains("it is not indexable"));

    // an unknown tab falls back to the hosts tab, and so do the tables
    // with pages of their own
    for tab in ["nonsense", "logs", "config"] {
        let (status, body) = get(&format!("{base}/hosts?tab={tab}")).await;
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
    assert!(running_page.contains("Running since"), "the run should be running");
    assert!(running_page.contains("Stop the running FediFetcher"));
    assert!(!running_page.contains("FediFetch NOW!"));

    let resp = client().post(format!("{base}/stop")).send().await.unwrap();
    assert_eq!(resp.status().as_u16(), 303);
    assert_eq!(resp.headers().get("Location").unwrap(), "/");

    // the run finishes early, and the history records that it was stopped
    let mut body = String::new();
    for _ in 0..100 {
        let (_, page) = get(&format!("{base}/")).await;
        if page.contains("Stopped.") {
            body = page;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert!(body.contains("Stopped."), "the run should report being stopped");
    assert!(body.contains(">stopped<"), "the outcome badge should say stopped");
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
        "server=example.social&new_access_token=smoke-secret-token-123&max_followers=5&from_lists=1&respect_robots_txt=0&http_timeout=9&hide_server_in_user_agent=1",
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

    // the page shows the saved values, and still masks the token
    let (status, body) = get(&format!("{base}/config")).await;
    assert_eq!(status, 200);
    assert!(body.contains("example.social"));
    assert!(body.contains(&masked(TOKEN)));
    assert!(!body.contains(TOKEN), "the token itself must not appear");
}

#[tokio::test]
async fn removing_a_token_via_the_config_page() {
    let dir = common::tmp_dir();
    let mut config = common::test_config(&dir, "example.social");
    config.access_tokens = vec!["first-token-aaaa".to_string(), "second-token-bbbb".to_string()];
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
        fedifetcher::db::load_config(&pool).await.unwrap().is_empty(),
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
    assert!(body.contains("example.social"), "names the configured server");
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
/// refresh, the one kind of live update that needs no JavaScript — and the
/// toggle pauses and resumes it, keeping the sort.
#[tokio::test]
async fn the_logs_page_refreshes_itself_unless_paused() {
    let dir = common::tmp_dir();
    let base = spawn_ui(&dir).await;

    let (status, body) = get(&format!("{base}/logs")).await;
    assert_eq!(status, 200);
    assert!(body.contains("http-equiv=\"refresh\""), "live by default");
    assert!(body.contains("content=\"5\""), "every five seconds");
    assert!(body.contains("Pause live updates"));

    let (status, body) = get(&format!("{base}/logs?live=0")).await;
    assert_eq!(status, 200);
    assert!(
        !body.contains("http-equiv=\"refresh\""),
        "paused: no auto-refresh"
    );
    assert!(body.contains("Resume live updates"));

    // the toggle keeps the sort and direction
    let (_, body) = get(&format!("{base}/logs?sort=level&dir=asc&live=0")).await;
    assert!(
        body.contains("/logs?sort=level&amp;dir=asc&amp;live=1"),
        "resuming keeps the sort: {body}"
    );
    let (_, body) = get(&format!("{base}/logs?sort=level&dir=asc")).await;
    assert!(
        body.contains("/logs?sort=level&amp;dir=asc&amp;live=0"),
        "pausing keeps the sort: {body}"
    );
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
            replies_count: Some(2),
            created_at: None,
            source: "home timeline".to_string(),
        }],
        1000,
    )
    .await
    .unwrap();
    let mut state = fedifetcher::store::State::default();
    state.known_followings.add("alice@remote.example", None);
    state.recently_checked_users.add("bob@remote.example", None);
    state.seen_urls.add("https://remote.example/@a/9", None);
    state
        .recently_checked_context
        .mark_fetched("https://remote.example/@a/5", fedifetcher::timeutil::now());
    state
        .seen_hosts
        .add("alpha.example", host_entry("Mastodon", "4.2.0", &["mastodon"], 3));
    fedifetcher::db::save_state(&pool, &state, fedifetcher::store::MAX_ENTRIES)
        .await
        .unwrap();
    let base = spawn_ui(&dir).await;

    // a post: fetch its context
    let (status, body) = get(&format!("{base}/hosts?tab=trigger_posts")).await;
    assert_eq!(status, 200);
    assert!(body.contains("action=\"/fetch\""), "the row posts to /fetch");
    assert!(body.contains("name=\"kind\" value=\"post\""));
    assert!(
        body.contains("name=\"key\" value=\"https://remote.example/@a/1\""),
        "the post's uri is the key"
    );
    assert!(body.contains(">Fetch context</button>"));
    assert!(
        body.contains("name=\"back\" value=\"/hosts?tab=trigger_posts&amp;sort=uri&amp;dir=asc\""),
        "the button returns to this view"
    );

    // a context cache row: same action
    let (status, body) = get(&format!("{base}/hosts?tab=context_cache")).await;
    assert_eq!(status, 200);
    assert!(
        body.contains("name=\"key\" value=\"https://remote.example/@a/5\""),
        "the cached context post is the key"
    );

    // a known following: backfill the user, into the followings set
    let (status, body) = get(&format!("{base}/hosts?tab=known_followings")).await;
    assert_eq!(status, 200);
    assert!(body.contains("name=\"kind\" value=\"user\""));
    assert!(body.contains("name=\"set\" value=\"followings\""));
    assert!(body.contains("name=\"key\" value=\"alice@remote.example\""));
    assert!(body.contains(">Backfill user</button>"));

    // a recently checked user: backfill, into the checked set
    let (status, body) = get(&format!("{base}/hosts?tab=recently_checked_users")).await;
    assert_eq!(status, 200);
    assert!(body.contains("name=\"set\" value=\"checked\""));
    assert!(body.contains("name=\"key\" value=\"bob@remote.example\""));

    // a seen URL: re-resolve it
    let (status, body) = get(&format!("{base}/hosts?tab=seen_urls")).await;
    assert_eq!(status, 200);
    assert!(body.contains("name=\"kind\" value=\"url\""));
    assert!(body.contains("name=\"key\" value=\"https://remote.example/@a/9\""));
    assert!(body.contains(">Re-resolve</button>"));

    // a host: re-check its server info
    let (status, body) = get(&format!("{base}/hosts")).await;
    assert_eq!(status, 200);
    assert!(body.contains("name=\"kind\" value=\"host\""));
    assert!(body.contains("name=\"key\" value=\"alpha.example\""));
    assert!(body.contains(">Re-check</button>"));

    // a table without an action has no fetch form
    let (status, body) = get(&format!("{base}/hosts?tab=private_posts")).await;
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
        "kind=url&key=https%3A%2F%2Fremote.example%2F%40a%2F1&back=%2Fhosts%3Ftab%3Dseen_urls",
    )
    .await;
    assert_eq!(status, 303, "the POST must redirect");
    assert_eq!(location.as_deref(), Some("/hosts?tab=seen_urls"));
}

/// The fetch POST's fields are validated: an unknown kind or an empty key
/// is not a fetch this UI offers, and the redirect target is always a
/// same-site path.
#[tokio::test]
async fn posting_fetch_validates_its_fields() {
    let dir = common::tmp_dir();
    let base = spawn_ui(&dir).await;

    // an unknown kind is not a fetch this UI offers
    let (status, location) = post_fetch(&base, "kind=dance&key=x&back=/hosts").await;
    assert_eq!(status, 303);
    assert_eq!(location.as_deref(), Some("/hosts"));

    // a fetch with no key does nothing
    let (status, location) = post_fetch(&base, "kind=post&key=&back=/hosts").await;
    assert_eq!(status, 303);
    assert_eq!(location.as_deref(), Some("/hosts"));

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
