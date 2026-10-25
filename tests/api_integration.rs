//! Choosing and speaking to the right server API. The flavor clients build
//! their request URLs with the scheme the HTTP client carries (https in
//! production, http under the test server), so these tests cover the parts
//! that need no network: which client a server gets, and how each flavor
//! reads its own URL shapes.

mod common;

use fedifetcher::api::{Client, client_for, find_post};
use fedifetcher::http::HttpClient;
use fedifetcher::servers::{ApiFlavor, ServerInfo};
use fedifetcher::store::State;
use fedifetcher::urls::PostRef;
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
        apis: BTreeSet::from([ApiFlavor::Misskey, ApiFlavor::Mastodon]),
        last_checked: fedifetcher::timeutil::now(),
        raw: json!({}),
    };
    assert!(matches!(
        client_for(&info, &http),
        Some(Client::Mastodon(_))
    ));
}

#[tokio::test]
async fn post_ids_are_read_in_each_flavors_own_url_shapes() {
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
    // /notes/ is a Misskey post shape. Iceshrimp hands it out too: it
    // speaks the Mastodon API, so the Mastodon client reads it as well
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
            .await
            .as_deref(),
        Some("abc123")
    );
    // a video URL is none of the Mastodon client's business
    assert_eq!(
        mastodon
            .post_id_from("https://example.social/w/abc123")
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
async fn usernames_are_read_in_each_flavors_own_url_shapes() {
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
    let mut state = State::default();

    let found = find_post("not a url", &mut state, &http).await;

    assert_eq!(found, None);
    // the URL is remembered as unreadable rather than asked about again
    assert_eq!(state.parsed_urls().get("not a url"), Some(&None));
}

#[test]
fn a_post_ref_reads_back_as_server_and_id() {
    let post_ref = PostRef::new("example.social", "12345");
    assert_eq!(post_ref.server, "example.social");
    assert_eq!(post_ref.post_id, "12345");
    assert_eq!(
        serde_json::to_string(&post_ref).unwrap(),
        r#"["example.social","12345"]"#
    );
}

/// A post on a server that is known but speaks none of our APIs cannot be
/// looked up, and the failure names the software it runs.
#[tokio::test]
async fn a_post_on_a_server_with_no_known_api_is_not_looked_up() {
    let dir = common::tmp_dir();
    let http = http(&dir);
    let mut state = State::default();
    let known = ServerInfo::from_nodeinfo(
        "remote.invalid",
        &json!({"software": {"name": "something-new", "version": "1"}}),
    )
    .unwrap();
    state
        .seen_hosts_mut()
        .add("remote.invalid", known.to_state());

    let found = find_post("https://remote.invalid/@alice/1", &mut state, &http).await;

    assert_eq!(found, None);
    // the answer is remembered, so the next scan does not ask again
    assert_eq!(
        state.parsed_urls().get("https://remote.invalid/@alice/1"),
        Some(&None)
    );
}

/// A canonical post URL on a server we already know is read from the URL
/// itself: no request is made, and the answer is remembered.
#[tokio::test]
async fn a_post_url_on_a_known_server_is_parsed_without_asking() {
    let dir = common::tmp_dir();
    let http = http(&dir);
    let mut state = State::default();
    state
        .seen_hosts_mut()
        .add("example.social", server_info("mastodon").to_state());

    let found = find_post("https://example.social/@alice/123", &mut state, &http).await;

    let found = found.expect("a canonical mastodon URL on a known server parses");
    assert_eq!(found.server, "example.social");
    assert_eq!(found.post_id, "123");
    assert_eq!(
        state.parsed_urls().get("https://example.social/@alice/123"),
        Some(&Some(found)),
        "the answer is remembered"
    );
}

/// Wafrn speaks the Mastodon API behind its own URL shapes: posts live at
/// /fediverse/post/<id>, profiles at /blog/<user>.
#[tokio::test]
async fn a_wafrn_servers_urls_are_read_by_the_mastodon_client() {
    let dir = common::tmp_dir();
    let http = http(&dir);
    let client = client_for(&server_info("wafrn"), &http)
        .expect("wafrn speaks the Mastodon API, so it gets a client");

    assert_eq!(
        client
            .post_id_from(
                "https://app.wafrn.net/fediverse/post/01a16b83-d93e-75e5-bc05-a563a5e5f1a3"
            )
            .await
            .as_deref(),
        Some("01a16b83-d93e-75e5-bc05-a563a5e5f1a3")
    );
    assert_eq!(
        client.username_from("https://app.wafrn.net/blog/someone"),
        Some("someone".to_string())
    );
}
