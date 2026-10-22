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
    spawn_ui_with(config, dir.join("config.toml")).await
}

/// Start the web UI with the given config and config file.
async fn spawn_ui_with(config: fedifetcher::config::Config, config_file: std::path::PathBuf) -> String {
    let db = fedifetcher::db::open(&config).await.unwrap();
    let app = web::app(config, config_file, db);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });
    format!("http://{addr}")
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

/// Read the saved config file back into a Config.
fn saved_config(path: &std::path::Path) -> fedifetcher::config::Config {
    let raw = fedifetcher::config::read_config_file(path.to_str().unwrap()).unwrap();
    fedifetcher::config::build_config(
        raw,
        fedifetcher::config::RawValues::new(),
        fedifetcher::config::RawValues::new(),
    )
    .unwrap()
}

#[tokio::test]
async fn every_page_renders_without_javascript() {
    let dir = common::tmp_dir();
    let base = spawn_ui(&dir).await;

    for (path, marker) in [
        ("/", "Run FediFetcher now"),
        ("/hosts", "Seen hosts"),
        ("/logs", "Recent log"),
        ("/config", "access_tokens"),
        ("/debug", "Test connection"),
    ] {
        let (status, body) = get(&format!("{base}{path}")).await;
        assert_eq!(status, 200, "{path}");
        assert!(body.contains(marker), "{path} should contain {marker:?}");
        assert!(body.contains("FediFetcher"), "{path} should name the app");
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
        body.contains("a line only the database knows"),
        "the page should render the database's lines"
    );
    assert!(
        body.contains("2026-10-22 16:05:07 -05:00: a line only the database knows"),
        "the line should be formatted like a live one"
    );
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
    assert!(body.contains("Run FediFetcher now"));
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
    let base = spawn_ui_with(config, dir.join("config.toml")).await;

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
    assert!(!running_page.contains("Run FediFetcher now"));

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
async fn saving_the_config_page_writes_the_toml_file() {
    let dir = common::tmp_dir();
    let mut config = common::test_config(&dir, "");
    config.access_tokens = Vec::new();
    let config_file = dir.join("config.toml");
    let base = spawn_ui_with(config, config_file.clone()).await;

    let (status, location, _) = post_config(
        &base,
        "server=example.social&new_access_token=smoke-secret-token-123&max_followers=5&from_lists=1&respect_robots_txt=0&http_timeout=9",
    )
    .await;
    assert_eq!(status, 303, "the POST must redirect");
    assert_eq!(location.as_deref(), Some("/config"));

    let saved = saved_config(&config_file);
    assert_eq!(saved.server, "example.social");
    assert_eq!(saved.access_tokens, vec![TOKEN.to_string()]);
    assert_eq!(saved.max_followers, 5);
    assert!(saved.from_lists);
    assert!(!saved.respect_robots_txt);
    assert_eq!(saved.http_timeout, 9);

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
    let config_file = dir.join("config.toml");
    let base = spawn_ui_with(config, config_file.clone()).await;

    let (status, _, _) = post_config(&base, "remove_access_token=0").await;
    assert_eq!(status, 303);

    let saved = saved_config(&config_file);
    assert_eq!(saved.access_tokens, vec!["second-token-bbbb".to_string()]);
}

#[tokio::test]
async fn invalid_values_are_rejected_and_not_saved() {
    let dir = common::tmp_dir();
    let config_file = dir.join("config.toml");
    let base = spawn_ui(&dir).await;

    let (status, _, body) = post_config(&base, "http_timeout=soon").await;
    assert_eq!(status, 400);
    assert!(body.contains("http_timeout"), "the error names the option");

    assert!(!config_file.exists(), "a failed save must not write the file");
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
    let base = spawn_ui_with(config, dir.join("config.toml")).await;

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
    let base = spawn_ui_with(config, dir.join("config.toml")).await;

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
