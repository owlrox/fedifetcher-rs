//! Posts found private: recorded in the database, and skipped without a
//! single request on later runs.

mod common;

use fedifetcher::api::mastodon::HomeServer;
use fedifetcher::context::known_context_urls;
use fedifetcher::db;
use fedifetcher::http::HttpClient;
use fedifetcher::posts::Post;
use fedifetcher::run::Context;
use fedifetcher::store::State;
use fedifetcher::timeutil;

/// A server that is not there: any request meant for it fails fast, which
/// is what tells "never touched" apart from "touched" below.
const REMOTE: &str = "127.0.0.1:9";

fn post(id: &str, is_public: bool) -> Post {
    Post {
        url: format!("http://{REMOTE}/@alice/{id}"),
        uri: format!("http://{REMOTE}/@alice/{id}"),
        created_at: timeutil::now(),
        is_public,
        reblog: None,
        in_reply_to_id: None,
        in_reply_to_account_id: None,
        reply_count: None,
        account: None,
        mentions: vec![],
    }
}

async fn context_urls(ctx: &mut Context<'_>, toot: &Post) -> Vec<String> {
    known_context_urls(ctx, std::slice::from_ref(toot), "test", false).await
}

/// find_post records every URL it resolves in parsed_urls, even when the
/// lookup fails, so an empty parsed_urls proves the post was never touched.
#[tokio::test]
async fn a_private_post_is_recorded_and_never_touched() {
    let dir = common::tmp_dir();
    let config = common::test_config(&dir, "home.example");
    let db = db::open(&config).await.unwrap();
    let http = HttpClient::new(config.clone(), db.clone());
    let home = HomeServer::new("home.example", "token", http.clone());
    let mut state = State::default();
    let mut ctx = common::run_context(&config, &mut state, &http, &home, &db);

    let toot = post("10", false);
    let urls = context_urls(&mut ctx, &toot).await;

    assert!(urls.is_empty(), "a private post has no context to find");
    assert!(
        state.parsed_urls.is_empty(),
        "not even the post's URL is resolved"
    );
    assert!(
        state.seen_hosts.is_empty(),
        "the post's server is not looked up"
    );
    assert!(
        db::is_private_post(&db, &toot.uri).await.unwrap(),
        "the post is recorded as private"
    );
}

/// The next run sees the same post again: the database says private, so the
/// toot is skipped even where this run's copy of it claims to be public.
#[tokio::test]
async fn a_post_marked_private_is_skipped_on_later_runs() {
    let dir = common::tmp_dir();
    let config = common::test_config(&dir, "home.example");
    let db = db::open(&config).await.unwrap();
    let http = HttpClient::new(config.clone(), db.clone());
    let home = HomeServer::new("home.example", "token", http.clone());
    let mut state = State::default();
    let mut ctx = common::run_context(&config, &mut state, &http, &home, &db);

    let toot = post("10", true);
    db::mark_private_post(&db, &toot.uri).await.unwrap();

    let urls = context_urls(&mut ctx, &toot).await;

    assert!(
        urls.is_empty(),
        "a post marked private is skipped whatever the toot says"
    );
    assert!(
        state.parsed_urls.is_empty(),
        "a recorded post is not resolved again, not even its URL"
    );
    assert!(
        state.seen_hosts.is_empty(),
        "the post's server is not looked up"
    );
}

/// The contrast that makes the two tests above mean something: a public,
/// unrecorded post is resolved like before.
#[tokio::test]
async fn a_public_post_is_still_resolved() {
    let dir = common::tmp_dir();
    let config = common::test_config(&dir, "home.example");
    let db = db::open(&config).await.unwrap();
    let http = HttpClient::new(config.clone(), db.clone());
    let home = HomeServer::new("home.example", "token", http.clone());
    let mut state = State::default();
    let mut ctx = common::run_context(&config, &mut state, &http, &home, &db);

    let toot = post("10", true);
    let _ = context_urls(&mut ctx, &toot).await;

    assert!(
        state.parsed_urls.contains_key(&toot.url),
        "a public post's URL is resolved like before"
    );
    assert!(
        !db::is_private_post(&db, &toot.uri).await.unwrap(),
        "a public post is not recorded as private"
    );
}

/// The summary line says what became of the toots, so a bare "Found 0"
/// explains itself: this one was skipped as private, that one's URL could
/// not be resolved.
#[tokio::test]
async fn the_summary_line_says_why_nothing_was_found() {
    let dir = common::tmp_dir();
    let config = common::test_config(&dir, "home.example");
    let db = db::open(&config).await.unwrap();
    let http = HttpClient::new(config.clone(), db.clone());
    let home = HomeServer::new("home.example", "token", http.clone());
    let mut state = State::default();
    let mut ctx = common::run_context(&config, &mut state, &http, &home, &db);

    let logs = common::LogCapture::new();
    let guard = logs.install();

    let private_toot = post("10", false);
    assert!(context_urls(&mut ctx, &private_toot).await.is_empty());
    let public_toot = post("11", true);
    assert!(context_urls(&mut ctx, &public_toot).await.is_empty());

    drop(guard);
    let log = logs.text();
    assert!(
        log.contains(
            "Found 0 known context toots (from 1 toots: 1 private, 0 unresolved, 0 already checked recently, 0 fetched)"
        ),
        "{log}"
    );
    assert!(
        log.contains(
            "Found 0 known context toots (from 1 toots: 0 private, 1 unresolved, 0 already checked recently, 0 fetched)"
        ),
        "{log}"
    );
}
