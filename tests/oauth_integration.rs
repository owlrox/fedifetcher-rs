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

/// The web UI playing its side of the dance: the tests below spawn it with
/// an http client pointed at the mock instance.
async fn spawn_ui(config: &Config, db: &sqlx::SqlitePool) -> String {
    let app = fedifetcher::web::app_with_scheme(config.clone(), db.clone(), "http");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });
    format!("http://{addr}")
}

/// A client that shows redirects, so the dance's steps can be walked one
/// at a time; the oauth_state cookie is carried by hand from each step's
/// Set-Cookie to the next step's Cookie header.
fn dance_client() -> reqwest::Client {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap()
}

/// The state cookie a dance step set, to carry into the next.
fn state_cookie(resp: &reqwest::Response) -> String {
    resp.headers()
        .get_all("set-cookie")
        .iter()
        .find_map(|value| {
            let text = value.to_str().ok()?;
            text.starts_with("ff_oauth_state=")
                .then(|| text.split(';').next().unwrap_or_default().to_string())
        })
        .expect("the dance sets the state cookie")
}

/// The instance answers the token exchange and names the account.
async fn mock_the_exchange(server: &MockServer) {
    server
        .mock_async(|when, then| {
            when.method(Method::POST)
                .path("/oauth/token")
                .body_includes("grant_type=authorization_code")
                .body_includes("code=the-dance-code")
                .body_includes("client_id=the-client-id");
            then.status(200)
                .json_body(serde_json::json!({ "access_token": "the-new-token" }));
        })
        .await;
    server
        .mock_async(|when, then| {
            when.method(Method::GET)
                .path("/api/v1/accounts/verify_credentials")
                .header("Authorization", "Bearer the-new-token");
            then.status(200).json_body(serde_json::json!({
                "acct": "alice",
                "avatar": "http://avatar.example/alice.png",
            }));
        })
        .await;
}

/// The access tokens as the database holds them.
async fn saved_tokens(db: &sqlx::SqlitePool) -> Vec<String> {
    let raw = fedifetcher::db::load_config(db).await.unwrap();
    raw.get("access_tokens")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// Walk the dance from the browser's seat: connect posts the server, the
/// instance approves, the callback trades the code, and the account lands
/// in the config exactly as a pasted token would.
#[tokio::test]
async fn the_full_dance_connects_an_account() {
    let server = MockServer::start_async().await;
    let dir = common::tmp_dir();
    let config = common::test_config(&dir, &host(&server));
    let db = fixture::test_db(&config).await;
    server
        .mock_async(|when, then| {
            when.method(Method::POST).path("/api/v1/apps");
            then.status(200).json_body(registration());
        })
        .await;
    mock_the_exchange(&server).await;

    let base = spawn_ui(&config, &db).await;
    let client = dance_client();

    // step 1: connect names the server and is sent to the instance
    let resp = client
        .post(format!("{base}/oauth/connect"))
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(format!("server={}", host(&server)))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status().as_u16(),
        303,
        "connect redirects to the instance"
    );
    let cookie = state_cookie(&resp);
    let authorize_url = resp.headers().get("Location").unwrap().to_str().unwrap();
    assert!(
        authorize_url.starts_with(&format!("http://{}/oauth/authorize", host(&server))),
        "{authorize_url}"
    );
    let authorize = url::Url::parse(authorize_url).unwrap();
    let state = authorize
        .query_pairs()
        .find(|(k, _)| k == "state")
        .map(|(_, v)| v.into_owned())
        .expect("the authorize URL carries the state");

    // step 2: the instance approved — its redirect is the callback with the
    // code and the very state the authorize URL carried
    let resp = client
        .get(format!(
            "{base}/oauth/callback?code=the-dance-code&state={state}"
        ))
        .header("Cookie", &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 303);
    assert_eq!(
        resp.headers().get("Location").unwrap(),
        "/config?oauth=connected"
    );
    // step 3: the account is connected — the token is in the config, and
    // the account row says who it belongs to
    let tokens = saved_tokens(&db).await;
    assert!(
        tokens.contains(&"the-new-token".to_string()),
        "the token joined access_tokens: {tokens:?}"
    );
    let row = fedifetcher::db::oauth_account(&db, "the-new-token")
        .await
        .unwrap()
        .expect("the connected account is recorded");
    assert_eq!(row.acct, "alice");
    assert_eq!(row.avatar, "http://avatar.example/alice.png");
    assert!(!row.needs_reconnect, "a fresh token needs no reconnect");
}

/// Consent denied on the instance lands back on the config page with
/// nothing added.
#[tokio::test]
async fn a_denied_consent_adds_nothing() {
    let server = MockServer::start_async().await;
    let dir = common::tmp_dir();
    let config = common::test_config(&dir, &host(&server));
    let db = fixture::test_db(&config).await;
    server
        .mock_async(|when, then| {
            when.method(Method::POST).path("/api/v1/apps");
            then.status(200).json_body(registration());
        })
        .await;

    let base = spawn_ui(&config, &db).await;
    let client = dance_client();

    let resp = client
        .post(format!("{base}/oauth/connect"))
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(format!("server={}", host(&server)))
        .send()
        .await
        .unwrap();
    let cookie = state_cookie(&resp);
    let authorize_url = resp.headers().get("Location").unwrap().to_str().unwrap();
    let authorize = url::Url::parse(authorize_url).unwrap();
    let state = authorize
        .query_pairs()
        .find(|(k, _)| k == "state")
        .map(|(_, v)| v.into_owned())
        .unwrap();

    // the instance says no
    let resp = client
        .get(format!(
            "{base}/oauth/callback?error=access_denied&state={state}"
        ))
        .header("Cookie", &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 303);
    assert_eq!(
        resp.headers().get("Location").unwrap(),
        "/config?oauth=denied",
        "the config page is told the consent was denied"
    );

    let tokens = saved_tokens(&db).await;
    assert!(
        tokens.is_empty(),
        "a denied consent adds no token: {tokens:?}"
    );
    assert!(
        fedifetcher::db::oauth_account(&db, "the-new-token")
            .await
            .unwrap()
            .is_none(),
        "and no account row"
    );
}

/// A callback whose state does not match the cookie is refused: the dance
/// was not started by us, and nothing is exchanged or added.
#[tokio::test]
async fn a_callback_with_a_foreign_state_adds_nothing() {
    let server = MockServer::start_async().await;
    let dir = common::tmp_dir();
    let config = common::test_config(&dir, &host(&server));
    let db = fixture::test_db(&config).await;

    let base = spawn_ui(&config, &db).await;
    let client = dance_client();

    // no connect preceded this: no state cookie exists at all
    let resp = client
        .get(format!(
            "{base}/oauth/callback?code=the-dance-code&state=forged-state"
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 303);
    assert_eq!(
        resp.headers().get("Location").unwrap(),
        "/config?oauth=expired",
        "the config page is told the dance is not ours"
    );

    let tokens = saved_tokens(&db).await;
    assert!(
        tokens.is_empty(),
        "a forged callback adds no token: {tokens:?}"
    );
    assert!(
        fedifetcher::db::oauth_account(&db, "the-new-token")
            .await
            .unwrap()
            .is_none(),
        "and no account row"
    );
}

/// Behind a reverse proxy, both ends of the dance must agree the callback
/// is on the PUBLIC origin: the registration's redirect_uris and the
/// authorize redirect's redirect_uri both follow the forwarded headers,
/// not the local address the UI actually listens on.
#[tokio::test]
async fn both_ends_of_the_dance_agree_on_the_public_origin_behind_a_proxy() {
    let server = MockServer::start_async().await;
    let dir = common::tmp_dir();
    let config = common::test_config(&dir, &host(&server));
    let db = fixture::test_db(&config).await;
    server
        .mock_async(move |when, then| {
            when.method(Method::POST).path("/api/v1/apps");
            then.status(200).json_body(registration());
        })
        .await;

    let base = spawn_ui(&config, &db).await;
    let client = dance_client();

    // the request arrives with the proxy's forwarding headers
    let resp = client
        .post(format!("{base}/oauth/connect"))
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(format!("server={}", host(&server)))
        .header("Host", "127.0.0.1:8080")
        .header("X-Forwarded-Proto", "https")
        .header("X-Forwarded-Host", "fetch.example")
        .send()
        .await
        .unwrap();
    let authorize_url = resp.headers().get("Location").unwrap().to_str().unwrap();

    // the instance was asked to register the callback on the public origin
    // (the registration mock's body_includes covers it in the earlier
    // test); here the authorize redirect must carry the same origin
    let authorize = url::Url::parse(authorize_url).unwrap();
    let redirect_uri = authorize
        .query_pairs()
        .find(|(k, _)| k == "redirect_uri")
        .map(|(_, v)| v.into_owned())
        .expect("the authorize URL carries the redirect_uri");
    assert_eq!(
        redirect_uri, "https://fetch.example/oauth/callback",
        "both ends of the dance name the public origin"
    );
}
