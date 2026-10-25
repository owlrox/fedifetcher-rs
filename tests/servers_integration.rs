//! Server discovery over the network: what a server runs, what its lookup
//! remembers, and what happens to the lookups that cannot be answered.

mod common;

use fedifetcher::http::HttpClient;
use fedifetcher::servers::{ApiFlavor, nodeinfo, server_info};
use fedifetcher::state::ServerCache;
use httpmock::Method;
use httpmock::prelude::*;
use serde_json::{Value, json};

/// A server that is not there yields no info, and the failure is cached with
/// its reason so the next scan does not ask again.
#[tokio::test]
async fn an_unreachable_server_is_remembered_as_a_failed_lookup() {
    let dir = common::tmp_dir();
    let config = common::test_config(&dir, "example.social");
    let db = fedifetcher::db::open(&config).await.unwrap();
    let http = HttpClient::new(config.clone(), db.clone());
    let mut seen_hosts = ServerCache::new();

    let first = server_info("127.0.0.1:9", &mut seen_hosts, &http).await;
    let second = server_info("127.0.0.1:9", &mut seen_hosts, &http).await;

    assert!(first.is_none(), "an unreachable server yields no info");
    assert!(second.is_none(), "and the cached failure says the same");
    let failure = seen_hosts
        .get("127.0.0.1:9")
        .expect("the failure is remembered");
    let reason = failure
        .get("error")
        .and_then(Value::as_str)
        .expect("the failure records its reason");
    assert!(
        reason.contains("Error getting host node info for 127.0.0.1:9"),
        "{reason}"
    );
}

/// The pieces the mock server serves for a discovery that succeeds: an
/// allow-everything robots.txt, a well-known nodeinfo pointing at its
/// document, and the document itself.
async fn mock_a_mastodon_server(server: &MockServer, software: &str, protocols: Value) {
    let host = host(server);
    server
        .mock_async(move |when, then| {
            when.method(Method::GET).path("/robots.txt");
            then.status(200).body("User-agent: *\nAllow: /\n");
        })
        .await;
    server
        .mock_async(move |when, then| {
            when.method(Method::GET).path("/.well-known/nodeinfo");
            then.status(200).json_body(json!({
                "links": [{
                    "rel": "http://nodeinfo.diaspora.software/ns/schema/2.0",
                    "href": format!("http://{host}/nodeinfo/2.0"),
                }],
            }));
        })
        .await;
    server
        .mock_async(move |when, then| {
            when.method(Method::GET).path("/nodeinfo/2.0");
            then.status(200).json_body(json!({
                "software": { "name": software, "version": "4.3.0" },
                "protocols": protocols,
            }));
        })
        .await;
}

/// The robots.txt and well-known of a server whose nodeinfo document is
/// given verbatim.
async fn mock_a_server_whose_document(server: &MockServer, document: Value) {
    let host = host(server);
    server
        .mock_async(move |when, then| {
            when.method(Method::GET).path("/robots.txt");
            then.status(200).body("User-agent: *\nAllow: /\n");
        })
        .await;
    server
        .mock_async(move |when, then| {
            when.method(Method::GET).path("/.well-known/nodeinfo");
            then.status(200).json_body(json!({
                "links": [{
                    "rel": "http://nodeinfo.diaspora.software/ns/schema/2.0",
                    "href": format!("http://{host}/nodeinfo/2.0"),
                }],
            }));
        })
        .await;
    server
        .mock_async(move |when, then| {
            when.method(Method::GET).path("/nodeinfo/2.0");
            then.status(200).json_body(document);
        })
        .await;
}

/// The mock server's host, without the scheme.
fn host(server: &MockServer) -> String {
    server.base_url().trim_start_matches("http://").to_string()
}

/// A discovery run against the mock server, with an empty cache.
async fn discover(
    server: &MockServer,
) -> (
    Result<fedifetcher::servers::ServerInfo, String>,
    ServerCache,
) {
    let dir = common::tmp_dir();
    let config = common::test_config(&dir, "example.social");
    let db = fedifetcher::db::open(&config).await.unwrap();
    let http = HttpClient::with_scheme(config, db, "http");
    let mut seen_hosts = ServerCache::new();
    let outcome = nodeinfo(&host(server), &mut seen_hosts, false, &http)
        .await
        .map_err(|err| err.to_string());
    (outcome, seen_hosts)
}

#[tokio::test]
async fn a_server_that_speaks_activitypub_announces_its_flavor() {
    let server = MockServer::start_async().await;
    mock_a_mastodon_server(&server, "mastodon", json!(["activitypub"])).await;

    let (outcome, _) = discover(&server).await;
    let info = outcome.expect("the mock server announces itself");
    assert_eq!(info.software, "mastodon");
    assert_eq!(info.version, "4.3.0");
    assert!(
        info.supports(ApiFlavor::Mastodon),
        "the announced software maps to its API flavor"
    );
    assert_eq!(
        info.webserver,
        host(&server),
        "the webserver comes from the nodeinfo document's location"
    );
}

#[tokio::test]
async fn a_lookup_is_cached_so_the_second_scan_asks_once() {
    let server = MockServer::start_async().await;
    mock_a_mastodon_server(&server, "mastodon", json!(["activitypub"])).await;

    let dir = common::tmp_dir();
    let config = common::test_config(&dir, "example.social");
    let db = fedifetcher::db::open(&config).await.unwrap();
    let http = HttpClient::with_scheme(config, db, "http");
    let mut seen_hosts = ServerCache::new();

    let first = server_info(&host(&server), &mut seen_hosts, &http)
        .await
        .expect("the first lookup succeeds");
    let second = server_info(&host(&server), &mut seen_hosts, &http)
        .await
        .expect("the cached lookup answers too");

    assert_eq!(first.software, second.software);
    assert!(
        seen_hosts.contains(&host(&server)),
        "the lookup is remembered in the cache"
    );
}

#[tokio::test]
async fn a_server_that_does_not_speak_activitypub_is_skipped() {
    let server = MockServer::start_async().await;
    mock_a_mastodon_server(&server, "mastodon", json!(["xmpp"])).await;

    let (outcome, _) = discover(&server).await;
    let err = outcome.expect_err("the announcement is checked");
    assert!(err.contains("does not support activitypub"), "{err}");
}

#[tokio::test]
async fn a_nodeinfo_document_without_a_software_name_is_an_error() {
    let server = MockServer::start_async().await;
    mock_a_server_whose_document(
        &server,
        json!({
            "software": { "version": "4.3.0" },
            "protocols": ["activitypub"],
        }),
    )
    .await;

    let (outcome, _) = discover(&server).await;
    let err = outcome.expect_err("a document without a software name is unreadable");
    assert!(err.contains("missing software name"), "{err}");
}

#[tokio::test]
async fn a_well_known_without_a_nodeinfo_link_is_an_error() {
    let server = MockServer::start_async().await;
    server
        .mock_async(move |when, then| {
            when.method(Method::GET).path("/robots.txt");
            then.status(200).body("User-agent: *\nAllow: /\n");
        })
        .await;
    server
        .mock_async(move |when, then| {
            when.method(Method::GET).path("/.well-known/nodeinfo");
            then.status(200).json_body(json!({ "links": [] }));
        })
        .await;

    let (outcome, _) = discover(&server).await;
    let err = outcome.expect_err("no link means no document to read");
    assert!(err.contains("could not find link to node info"), "{err}");
}

#[tokio::test]
async fn a_well_known_that_is_not_json_is_an_error() {
    let server = MockServer::start_async().await;
    server
        .mock_async(|when, then| {
            when.method(Method::GET).path("/robots.txt");
            then.status(200).body("User-agent: *\nAllow: /\n");
        })
        .await;
    server
        .mock_async(|when, then| {
            when.method(Method::GET).path("/.well-known/nodeinfo");
            then.status(200).body("not json at all");
        })
        .await;

    let (outcome, _) = discover(&server).await;
    let err = outcome.expect_err("an unreadable well-known is reported");
    assert!(err.contains("well-known"), "{err}");
}

#[tokio::test]
async fn a_nodeinfo_document_that_is_not_there_is_reported() {
    let server = MockServer::start_async().await;
    let host = host(&server);
    server
        .mock_async(move |when, then| {
            when.method(Method::GET).path("/robots.txt");
            then.status(200).body("User-agent: *\nAllow: /\n");
        })
        .await;
    server
        .mock_async(move |when, then| {
            when.method(Method::GET).path("/.well-known/nodeinfo");
            then.status(200).json_body(json!({
                "links": [{
                    "rel": "http://nodeinfo.diaspora.software/ns/schema/2.0",
                    "href": format!("http://{host}/nodeinfo/2.0"),
                }],
            }));
        })
        .await;
    server
        .mock_async(|when, then| {
            when.method(Method::GET).path("/nodeinfo/2.0");
            then.status(500);
        })
        .await;

    let (outcome, _) = discover(&server).await;
    let err = outcome.expect_err("a failed document fetch is reported");
    assert!(err.contains("Status Code: 500"), "{err}");
}

#[tokio::test]
async fn an_href_without_a_server_in_it_is_reported() {
    let server = MockServer::start_async().await;
    server
        .mock_async(|when, then| {
            when.method(Method::GET).path("/robots.txt");
            then.status(200).body("User-agent: *\nAllow: /\n");
        })
        .await;
    server
        .mock_async(|when, then| {
            when.method(Method::GET).path("/.well-known/nodeinfo");
            then.status(200).json_body(json!({
                "links": [{
                    "rel": "http://nodeinfo.diaspora.software/ns/schema/2.0",
                    "href": "not a url",
                }],
            }));
        })
        .await;

    let (outcome, _) = discover(&server).await;
    let err = outcome.expect_err("an href without a server cannot name the webserver");
    assert!(err.contains("Error getting web server name"), "{err}");
}

/// A well-known that answers 404 sends the lookup to the host-meta document,
/// which names the webfinger server the real ask should go to.
async fn mock_host_meta(server: &MockServer, template: String) {
    server
        .mock_async(move |when, then| {
            when.method(Method::GET).path("/.well-known/host-meta");
            then.status(200).body(format!(
                r#"<?xml version="1.0" encoding="UTF-8"?>
<XRD xmlns="http://docs.oasis-open.org/ns/xri/xrd-1.0">
  <Link rel="lrdd" template="{template}"/>
</XRD>"#
            ));
        })
        .await;
}

#[tokio::test]
async fn a_well_known_404_falls_back_to_the_host_meta_server() {
    let server = MockServer::start_async().await;
    // no well-known mock: the request draws a 404, and the fallback begins
    mock_host_meta(
        &server,
        format!("http://{}/webfinger?q={{uri}}", "fake-web.invalid"),
    )
    .await;

    let (outcome, _) = discover(&server).await;
    let err = outcome.expect_err("the fallback's server is not there");
    assert!(
        err.contains("fake-web.invalid"),
        "the lookup moved on to the host-meta's server: {err}"
    );
}

#[tokio::test]
async fn a_host_meta_that_names_the_same_server_is_an_error() {
    let server = MockServer::start_async().await;
    mock_host_meta(
        &server,
        format!("http://{}/webfinger?q={{uri}}", host(&server)),
    )
    .await;

    let (outcome, _) = discover(&server).await;
    let err = outcome.expect_err("a self-referencing host-meta cannot redirect the lookup");
    assert!(
        err.contains("did not identify a different web server"),
        "{err}"
    );
}

#[tokio::test]
async fn a_well_known_404_without_a_host_meta_reports_the_well_known_status() {
    let server = MockServer::start_async().await;
    // nothing is mocked: both the well-known and the host-meta draw 404s

    let (outcome, _) = discover(&server).await;
    let err = outcome.expect_err("neither document exists");
    assert!(
        err.contains("no nodeinfo found for") && err.contains("404"),
        "{err}"
    );
}
