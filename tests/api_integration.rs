//! Choosing and speaking to the right server API. The flavour clients build
//! their request URLs with a hardcoded https scheme (as the Python original
//! does), so these tests cover the parts that need no network: which client a
//! server gets, and how each flavour reads its own URL shapes.

mod common;

use fedifetcher::api::{Client, client_for, find_post};
use fedifetcher::http::HttpClient;
use fedifetcher::servers::{ApiFlavour, ServerInfo};
use fedifetcher::state::ServerCache;
use fedifetcher::urls::PostRef;
use indexmap::IndexMap;
use serde_json::json;
use std::collections::BTreeSet;

fn server_info(software: &str) -> ServerInfo {
    ServerInfo::from_nodeinfo(
        "example.social",
        &json!({
            "protocols": ["activitypub"],
            "software": {"name": software, "version": "1"},
        }),
    )
    .expect("a nodeinfo with a software name should give a ServerInfo")
}

fn http(dir: &std::path::Path) -> HttpClient {
    // these tests need no network, so the database is never queried; a lazy
    // in-memory pool stands in for the real one
    let config = common::test_config(dir, "example.social");
    let db = sqlx::sqlite::SqlitePoolOptions::new()
        .connect_lazy("sqlite::memory:")
        .unwrap();
    HttpClient::new(config, db)
}

#[tokio::test]
async fn a_client_is_chosen_by_what_the_server_speaks() {
    let dir = common::tmp_dir();
    let http = http(&dir);
    assert!(matches!(
        client_for(&server_info("mastodon"), &http),
        Some(Client::Mastodon(_))
    ));
    assert!(matches!(
        client_for(&server_info("lemmy"), &http),
        Some(Client::Lemmy(_))
    ));
    assert!(matches!(
        client_for(&server_info("misskey"), &http),
        Some(Client::Misskey(_))
    ));
    assert!(matches!(
        client_for(&server_info("peertube"), &http),
        Some(Client::PeerTube(_))
    ));
}

#[tokio::test]
async fn a_server_we_cannot_talk_to_gets_no_client() {
    let dir = common::tmp_dir();
    let http = http(&dir);
    assert!(client_for(&server_info("something-new"), &http).is_none());
}

#[tokio::test]
async fn mastodon_wins_when_a_server_claims_more_than_one_api() {
    let dir = common::tmp_dir();
    let http = http(&dir);
    let info = ServerInfo {
        webserver: "example.social".to_string(),
        software: "hybrid".to_string(),
        version: "1".to_string(),
        apis: BTreeSet::from([ApiFlavour::Misskey, ApiFlavour::Mastodon]),
        last_checked: fedifetcher::timeutil::now(),
        raw: json!({}),
    };
    assert!(matches!(
        client_for(&info, &http),
        Some(Client::Mastodon(_))
    ));
}

#[tokio::test]
async fn post_ids_are_read_in_each_flavours_own_url_shapes() {
    let dir = common::tmp_dir();
    let http = http(&dir);
    let mastodon = client_for(&server_info("mastodon"), &http).unwrap();
    let misskey = client_for(&server_info("misskey"), &http).unwrap();
    let lemmy = client_for(&server_info("lemmy"), &http).unwrap();
    let peertube = client_for(&server_info("peertube"), &http).unwrap();

    assert_eq!(
        mastodon
            .post_id_from("https://example.social/@someone/12345")
            .await
            .as_deref(),
        Some("12345")
    );
    // /notes/ is a Misskey post; on a Mastodon server it is nothing
    assert_eq!(
        misskey
            .post_id_from("https://example.social/notes/abc123")
            .await
            .as_deref(),
        Some("abc123")
    );
    assert_eq!(
        mastodon
            .post_id_from("https://example.social/notes/abc123")
            .await,
        None
    );
    assert_eq!(
        lemmy
            .post_id_from("https://example.social/post/42")
            .await
            .as_deref(),
        Some("42")
    );
    assert_eq!(
        peertube
            .post_id_from("https://example.social/w/abc123")
            .await
            .as_deref(),
        Some("abc123")
    );
}

#[tokio::test]
async fn usernames_are_read_in_each_flavours_own_url_shapes() {
    let dir = common::tmp_dir();
    let http = http(&dir);
    let mastodon = client_for(&server_info("mastodon"), &http).unwrap();
    let misskey = client_for(&server_info("misskey"), &http).unwrap();
    let lemmy = client_for(&server_info("lemmy"), &http).unwrap();
    let peertube = client_for(&server_info("peertube"), &http).unwrap();

    assert_eq!(
        mastodon.username_from("https://example.social/@someone"),
        Some("someone".to_string())
    );
    assert_eq!(
        misskey.username_from("https://example.social/@someone"),
        Some("someone".to_string())
    );
    assert_eq!(
        lemmy.username_from("https://example.social/u/someone"),
        Some("someone".to_string())
    );
    assert_eq!(
        peertube.username_from("https://example.social/a/someone"),
        Some("someone".to_string())
    );
    // Mastodon also reads Pixelfed's prefix-less profiles
    assert_eq!(
        mastodon.username_from("https://example.social/someone"),
        Some("someone".to_string())
    );
    // but each client only answers for its own shapes
    assert_eq!(
        mastodon.username_from("https://example.social/u/someone"),
        None
    );
    assert_eq!(lemmy.username_from("https://example.social/@someone"), None);
}

#[tokio::test]
async fn a_url_with_no_host_is_not_looked_up() {
    let dir = common::tmp_dir();
    let http = http(&dir);
    let mut parsed = IndexMap::new();
    let mut seen_hosts = ServerCache::new();

    let found = find_post("not a url", &mut parsed, &mut seen_hosts, &http).await;

    assert_eq!(found, None);
    // the URL is remembered as unreadable rather than asked about again
    assert_eq!(parsed.get("not a url"), Some(&None));
}

#[test]
fn a_post_ref_reads_back_as_server_and_id() {
    let post_ref = PostRef::new("example.social", "12345");
    assert_eq!(post_ref.0, "example.social");
    assert_eq!(post_ref.1, "12345");
    assert_eq!(
        serde_json::to_string(&post_ref).unwrap(),
        r#"["example.social","12345"]"#
    );
}
