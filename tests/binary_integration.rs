//! The real binary, as a process: the command line's two modes, the web
//! UI's startup, and what serve does to a run history it finds on disk.
//! The tests point the child at its own state directory through
//! XDG_STATE_HOME — with HOME pointed into the same sandbox behind it —
//! pre-seed its database where a test needs to, and assert on what the
//! child left behind. The sandboxing is canaried: if default_state_dir's
//! precedence ever changes, the tests say so instead of failing on a
//! missing row, and nothing is written into the developer's real state.

use std::process::Stdio;
use std::time::Duration;

use fedifetcher::config::Config;
use fedifetcher::db;
use fedifetcher::db::{LogOrder, LogSort};
use tokio::process::Child;

/// A fresh temporary directory, unique to this test.
fn tmp_dir() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "fedifetcher-binary-test-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// The state directory the child process will open: default_state_dir
/// honors XDG_STATE_HOME, and no artifacts/ directory exists in the child's
/// working directory.
fn state_dir(tmp: &std::path::Path) -> std::path::PathBuf {
    tmp.join("xdg").join("fedifetcher")
}

/// Open the database the child will open, creating the schema.
async fn state_db(tmp: &std::path::Path) -> (Config, sqlx::SqlitePool) {
    let config = Config {
        state_dir: state_dir(tmp),
        ..Default::default()
    };
    let db = db::open(&config).await.unwrap();
    (config, db)
}

/// The binary, run as a child process with its own state home. HOME is
/// pointed into the sandbox too: default_state_dir falls back to
/// ~/.local/state/fedifetcher when XDG_STATE_HOME is not honored, and that
/// fallback must land in the test's own directory, never in the real one.
fn child(tmp: &std::path::Path, args: &[&str]) -> tokio::process::Command {
    let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_fedifetcher"));
    command
        .args(args)
        .env("XDG_STATE_HOME", tmp.join("xdg"))
        .env("HOME", tmp.join("home"))
        .current_dir(tmp)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command
}

/// The child must have resolved its state through XDG_STATE_HOME: neither
/// the legacy artifacts/ directory of its working directory (which
/// default_state_dir prefers over everything else) nor its HOME fallback
/// may hold state. If either fires, the seeding in these tests no longer
/// matches what the binary opens, and the test says which assumption
/// broke instead of failing later on a missing row.
fn assert_state_stayed_in_the_sandbox(tmp: &std::path::Path) {
    assert!(
        !tmp.join("artifacts").exists(),
        "the child resolved its state to the legacy artifacts/ directory in \
         its working directory: default_state_dir's precedence changed, and \
         these tests seed a different database than the binary opens"
    );
    assert!(
        !tmp.join("home").join(".local").exists(),
        "the child resolved its state through HOME: XDG_STATE_HOME is not \
         being honored, and these tests seed a different database than the \
         binary opens"
    );
}

/// A port nothing is listening on yet.
fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// Poll until the web UI answers /healthz with `ok`, and return the body.
async fn healthz(base: &str) -> String {
    let client = reqwest::Client::new();
    for _ in 0..200 {
        if let Ok(resp) = client.get(format!("{base}/healthz")).send().await
            && resp.status().is_success()
        {
            return resp.text().await.unwrap();
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("the web UI never answered {base}/healthz");
}

/// Stop a spawned web UI, say whether it stayed up until we did, and that
/// its state stayed in the sandbox.
async fn stop_ui(tmp: &std::path::Path, mut ui: Child) {
    ui.kill().await.unwrap();
    let status = ui.wait().await.unwrap();
    assert!(
        status.code().is_none() || !status.success(),
        "the web UI should have been killed, not exited on its own"
    );
    assert_state_stayed_in_the_sandbox(tmp);
}

#[tokio::test]
async fn the_version_flag_prints_the_version() {
    let tmp = tmp_dir();
    let output = child(&tmp, &["--version"]).output().await.unwrap();
    assert!(output.status.success(), "{output:?}");
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(
        text.contains(env!("CARGO_PKG_VERSION")),
        "--version names the version: {text}"
    );
}

/// A saved config with nothing enabled: the run finishes immediately,
/// records itself in the history, and the process exits clean.
#[tokio::test]
async fn a_run_with_a_saved_config_runs_once_and_exits_clean() {
    let tmp = tmp_dir();
    let (config, db) = state_db(&tmp).await;
    let saved = Config {
        server: "example.social".to_string(),
        access_tokens: vec!["token".to_string()],
        ..config.clone()
    };
    db::save_config(&db, &saved.to_raw_values()).await.unwrap();
    drop(db);

    let output = child(&tmp, &["run"]).output().await.unwrap();
    assert!(
        output.status.success(),
        "the run exits clean: {:?} / stderr {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );

    assert_state_stayed_in_the_sandbox(&tmp);
    let db = db::open(&config).await.unwrap();
    let runs = db::recent_runs(&db, 10).await.unwrap();
    assert_eq!(runs.len(), 1, "the run recorded itself in the history");
    assert_eq!(runs[0].outcome, "ok");
    assert!(
        runs[0].message.starts_with("Processing finished in"),
        "{}",
        runs[0].message
    );
}

/// Running without a configured server or token is refused before anything
/// is contacted, with a critical log and a failed exit.
#[tokio::test]
async fn a_run_without_a_config_is_refused() {
    let tmp = tmp_dir();
    let _ = state_db(&tmp).await;

    let output = child(&tmp, &["run"]).output().await.unwrap();
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.starts_with("CRITICAL:FediFetcher:"),
        "the refusal is critical: {stderr}"
    );
    assert_state_stayed_in_the_sandbox(&tmp);
}

/// The web UI answers when no configuration exists at all (the config page
/// is how it gets one), and a run row left behind by a dead process is
/// marked interrupted during startup.
#[tokio::test]
async fn serve_answers_without_a_config_and_sweeps_a_dead_runs_row() {
    let tmp = tmp_dir();
    let (config, db) = state_db(&tmp).await;
    // a run row from a process that is long gone
    db::start_run(
        &db,
        &fedifetcher::timeutil::fmt_state_ts(fedifetcher::timeutil::now()),
    )
    .await
    .unwrap();
    drop(db);

    let port = free_port();
    let ui = child(&tmp, &["serve", "--port", &port.to_string()])
        .spawn()
        .unwrap();
    let body = healthz(&format!("http://127.0.0.1:{port}")).await;
    assert_eq!(body, "ok");

    // the sweep ran before the listener came up: the dead run is history now
    let db = db::open(&config).await.unwrap();
    let runs = db::recent_runs(&db, 10).await.unwrap();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].outcome, "error");
    assert!(
        runs[0].message.contains("Interrupted"),
        "the dead run explains itself: {}",
        runs[0].message
    );
    drop(db);

    stop_ui(&tmp, ui).await;
}

/// Streaming without a configured server or token is refused with a warning,
/// and the web UI still comes up.
#[tokio::test]
async fn serve_warns_and_skips_streaming_without_a_server() {
    let tmp = tmp_dir();
    let (config, db) = state_db(&tmp).await;
    let mut saved = Config {
        streaming: true,
        ..config.clone()
    };
    saved.server = String::new();
    saved.access_tokens = Vec::new();
    db::save_config(&db, &saved.to_raw_values()).await.unwrap();
    drop(db);

    let port = free_port();
    let ui = child(&tmp, &["serve", "--port", &port.to_string()])
        .spawn()
        .unwrap();
    let body = healthz(&format!("http://127.0.0.1:{port}")).await;
    assert_eq!(body, "ok");

    // the warning reached the database the logs page reads
    let db = db::open(&config).await.unwrap();
    for _ in 0..100 {
        let rows = db::recent_logs(&db, 100, LogSort::Time, LogOrder::Desc, None)
            .await
            .unwrap();
        if rows.iter().any(|row| row.message.contains("not streaming")) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let rows = db::recent_logs(&db, 100, LogSort::Time, LogOrder::Desc, None)
        .await
        .unwrap();
    assert!(
        rows.iter().any(|row| row.message.contains("not streaming")),
        "the streaming refusal is logged: {:?}",
        rows.iter().map(|row| &row.message).collect::<Vec<_>>()
    );
    drop(db);

    stop_ui(&tmp, ui).await;
}

/// Streaming with a server and token starts the listener.
#[tokio::test]
async fn serve_starts_the_stream_listener_when_the_config_asks() {
    let tmp = tmp_dir();
    let (config, db) = state_db(&tmp).await;
    // the server is unreachable: the listener fails and reconnects on its
    // growing delays, which is fine — the spawn is what this covers
    let saved = Config {
        server: "127.0.0.1:9".to_string(),
        access_tokens: vec!["token".to_string()],
        streaming: true,
        ..config.clone()
    };
    db::save_config(&db, &saved.to_raw_values()).await.unwrap();
    drop(db);

    let port = free_port();
    let ui = child(&tmp, &["serve", "--port", &port.to_string()])
        .spawn()
        .unwrap();
    let body = healthz(&format!("http://127.0.0.1:{port}")).await;
    assert_eq!(body, "ok");
    stop_ui(&tmp, ui).await;
}

/// A port that is already taken is reported as an error, with a failed exit.
#[tokio::test]
async fn serve_on_a_taken_port_fails_with_the_reason() {
    let tmp = tmp_dir();
    let _ = state_db(&tmp).await;
    let port = free_port();
    // hold the port ourselves
    let held = std::net::TcpListener::bind(format!("127.0.0.1:{port}")).unwrap();

    let output = child(&tmp, &["serve", "--port", &port.to_string()])
        .output()
        .await
        .unwrap();
    drop(held);

    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("ERROR:FediFetcher:") && stderr.contains("os error"),
        "the bind failure names itself: {stderr}"
    );
    assert_state_stayed_in_the_sandbox(&tmp);
}
