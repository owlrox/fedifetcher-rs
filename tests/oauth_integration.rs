//! The OAuth dance against httpmock playing the instance: app registration
//! and the authorize redirect (Task 1), then the callback and exchange
//! (Task 2).

mod common;
mod fixture;

use fedifetcher::config::Config;
use fedifetcher::http::HttpClient;
use fedifetcher::oauth;
use httpmock::Method;
use httpmock::prelude::*;

/// The mock server's host, without the scheme.
fn host(server: &MockServer) -> String {
    server.base_url().trim_start_matches("http://").to_string()
}

/// An http client pointed at the mock server, like every suite that plays
/// the instance over http.
fn http(config: &Config, db: &sqlx::SqlitePool) -> HttpClient {
    HttpClient::with_scheme(config.clone(), db.clone(), "http")
}

/// The app registration the instance answers with.
fn registration() -> serde_json::Value {
    serde_json::json!({
        "id": "17",
        "client_id": "the-client-id",
        "client_secret": "the-client-secret",
    })
}

/// The registration request carries the app's name, the callback the
/// instance must redirect to, and the scopes FediFetcher needs.
#[tokio::test]
async fn a_new_server_is_registered_with_name_callback_and_scopes() {
    let server = MockServer::start_async().await;
    let dir = common::tmp_dir();
    let config = common::test_config(&dir, &host(&server));
    let db = fixture::test_db(&config).await;
    let http = http(&config, &db);

    let mock_host = host(&server);
    // the form encoding percent-encodes everything, the port's colon too
    let encoded_host = mock_host.replace(':', "%3A");
    let registration_mock = server
        .mock_async(move |when, then| {
            when.method(Method::POST)
                .path("/api/v1/apps")
                .body_includes("client_name=FediFetcher-rs")
                .body_includes(format!("redirect_uris=http%3A%2F%2F{encoded_host}").as_str())
                .body_includes("%2Foauth%2Fcallback")
                .body_includes("read%3Asearch+read%3Astatuses+admin%3Aread%3Aaccounts");
            then.status(200).json_body(registration());
        })
        .await;

    let origin = format!("http://{}", host(&server));
    let app = oauth::registered_app(&db, &host(&server), &http, &origin)
        .await
        .expect("the registration answers");

    assert_eq!(app.client_id, "the-client-id");
    assert_eq!(registration_mock.calls(), 1);
}

/// A second ask for the same server reuses the cached registration: the
/// instance is asked to register the app once, not once per visit.
#[tokio::test]
async fn a_registered_server_is_not_registered_again() {
    let server = MockServer::start_async().await;
    let dir = common::tmp_dir();
    let config = common::test_config(&dir, &host(&server));
    let db = fixture::test_db(&config).await;
    let http = http(&config, &db);

    let registration_mock = server
        .mock_async(|when, then| {
            when.method(Method::POST).path("/api/v1/apps");
            then.status(200).json_body(registration());
        })
        .await;

    let origin = format!("http://{}", host(&server));
    let first = oauth::registered_app(&db, &host(&server), &http, &origin)
        .await
        .expect("the first registration answers");
    let second = oauth::registered_app(&db, &host(&server), &http, &origin)
        .await
        .expect("the cached registration answers");

    assert_eq!(first.client_id, second.client_id);
    assert_eq!(registration_mock.calls(), 1, "asked once");
}

/// The authorize URL is the instance's own consent screen, pointed at with
/// every parameter the dance needs.
#[tokio::test]
async fn the_authorize_url_carries_every_parameter_the_dance_needs() {
    let server = MockServer::start_async().await;
    let dir = common::tmp_dir();
    let config = common::test_config(&dir, &host(&server));
    let db = fixture::test_db(&config).await;
    let http = http(&config, &db);

    server
        .mock_async(|when, then| {
            when.method(Method::POST).path("/api/v1/apps");
            then.status(200).json_body(registration());
        })
        .await;

    let origin = format!("http://{}", host(&server));
    let app = oauth::registered_app(&db, &host(&server), &http, &origin)
        .await
        .expect("the registration answers");

    let url = oauth::authorize_url(&app, "http", &origin, "the-state-value");
    let url = url::Url::parse(&url).expect("the authorize URL parses");

    let with_port = match url.port() {
        Some(port) => format!("{}:{port}", url.host_str().unwrap_or_default()),
        None => url.host_str().unwrap_or_default().to_string(),
    };
    assert_eq!(with_port, host(&server));
    assert_eq!(url.path(), "/oauth/authorize");
    let pairs: std::collections::BTreeMap<String, String> = url
        .query_pairs()
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    assert_eq!(pairs.get("client_id").unwrap(), "the-client-id");
    assert_eq!(pairs.get("response_type").unwrap(), "code");
    assert_eq!(pairs.get("state").unwrap(), "the-state-value");
    assert_eq!(
        pairs.get("redirect_uri").unwrap(),
        &format!("{origin}/oauth/callback")
    );
    assert_eq!(
        pairs.get("scope").unwrap(),
        "read:search read:statuses admin:read:accounts"
    );
}

/// The public origin the callback is built from follows the forwarding
/// headers a reverse proxy sets, falling back to the request's own host.
#[tokio::test]
async fn the_public_origin_follows_the_forwarding_headers() {
    use reqwest::header::{HeaderMap, HeaderName, HeaderValue};

    let headers = |pairs: &[(&str, &str)]| {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.insert(
                HeaderName::from_bytes(name.as_bytes()).unwrap(),
                HeaderValue::from_str(value).unwrap(),
            );
        }
        map
    };

    // a plain local request: the Host header alone, http
    assert_eq!(
        oauth::public_origin(&headers(&[("Host", "127.0.0.1:8080")])),
        "http://127.0.0.1:8080"
    );
    // behind a proxy that forwards: the public scheme and host win
    assert_eq!(
        oauth::public_origin(&headers(&[
            ("Host", "127.0.0.1:8080"),
            ("X-Forwarded-Proto", "https"),
            ("X-Forwarded-Host", "fetch.example"),
        ])),
        "https://fetch.example"
    );
}
