//! Speaking an instance's OAuth authorization-code flow: registering the
//! app on the instance, building the authorize redirect the user follows,
//! and — with the callback route — trading the approval code for an access
//! token. Three HTTPS calls and a redirect; no OAuth crate.

use serde_json::Value;
use sqlx::SqlitePool;
use tracing::info;

use crate::db;
use crate::error::{Error, Result};
use crate::http::HttpClient;

/// The permissions FediFetcher asks the instance for, one consent screen.
/// One more than the fetching needs: `read:accounts` is verify_credentials's
/// price, and the dance asks whose token it just minted.
pub const SCOPES: &str = "read:search read:statuses admin:read:accounts read:accounts";

/// The app as an instance knows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RegisteredApp {
    pub server: String,
    pub client_id: String,
    pub client_secret: String,
}

/// The app registered on `server` — from the cache when it is registered
/// there already, by asking the instance otherwise. `origin` is the public
/// origin the instance must redirect back to, derived the same way at both
/// ends of the dance.
pub async fn registered_app(
    db: &SqlitePool,
    server: &str,
    http: &HttpClient,
    origin: &str,
) -> Result<RegisteredApp> {
    // a cached registration is reused only when its scope ask still is
    // ours: the instance validates the authorize-time scopes against the
    // registered ones, so a stale registration means "invalid scope"
    if let Some(cached) = db::oauth_app(db, server).await? {
        if cached.scopes == SCOPES {
            return Ok(RegisteredApp {
                server: server.to_string(),
                client_id: cached.client_id,
                client_secret: cached.client_secret,
            });
        }
        info!("The app registered on {server} asks for different scopes; registering again");
    }

    let form = [
        ("client_name".to_string(), "FediFetcher-rs".to_string()),
        (
            "redirect_uris".to_string(),
            format!("{origin}/oauth/callback"),
        ),
        ("scopes".to_string(), SCOPES.to_string()),
    ];
    let url = format!("{scheme}://{server}/api/v1/apps", scheme = http.scheme());
    let resp = http.post_form(&url, &form, None).await?;
    if resp.status != 200 {
        return Err(Error::Fetch(format!(
            "registering the app on {server} failed with status {}",
            resp.status
        )));
    }
    let answer: Value = resp.json()?;
    let client_id = answer
        .get("client_id")
        .and_then(Value::as_str)
        .ok_or_else(|| Error::Fetch("the registration answer has no client_id".to_string()))?;
    let client_secret = answer
        .get("client_secret")
        .and_then(Value::as_str)
        .ok_or_else(|| Error::Fetch("the registration answer has no client_secret".to_string()))?;

    let app = RegisteredApp {
        server: server.to_string(),
        client_id: client_id.to_string(),
        client_secret: client_secret.to_string(),
    };
    db::save_oauth_app(db, &app.server, &app.client_id, &app.client_secret, SCOPES).await?;
    Ok(app)
}

/// The account a token belongs to, as the instance spells it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConnectedAccount {
    pub acct: String,
    pub avatar: String,
}

/// Trade the approval code for an access token, and learn whose it is.
pub async fn exchange_code(
    http: &HttpClient,
    app: &RegisteredApp,
    origin: &str,
    code: &str,
) -> Result<(String, ConnectedAccount)> {
    let form = [
        ("client_id".to_string(), app.client_id.clone()),
        ("client_secret".to_string(), app.client_secret.clone()),
        (
            "redirect_uri".to_string(),
            format!("{origin}/oauth/callback"),
        ),
        ("grant_type".to_string(), "authorization_code".to_string()),
        ("code".to_string(), code.to_string()),
    ];
    let url = format!(
        "{scheme}://{server}/oauth/token",
        scheme = http.scheme(),
        server = app.server
    );
    let resp = http.post_form(&url, &form, None).await?;
    if resp.status != 200 {
        return Err(Error::Fetch(format!(
            "trading the approval code for a token failed with status {}",
            resp.status
        )));
    }
    let answer: Value = resp.json()?;
    let token = answer
        .get("access_token")
        .and_then(Value::as_str)
        .ok_or_else(|| Error::Fetch("the token answer has no access_token".to_string()))?;

    // whose token is it? the fresh token travels with the ask — the
    // instance names an account only to a bearer it just minted
    let authed = http.authenticated(token);
    let who = authed
        .get(
            &format!(
                "{scheme}://{server}/api/v1/accounts/verify_credentials",
                scheme = http.scheme(),
                server = app.server
            ),
            None,
        )
        .await?;
    if who.status != 200 {
        return Err(Error::Fetch(format!(
            "checking whose token it is failed with status {}",
            who.status
        )));
    }
    let who: Value = who.json()?;
    let acct = who
        .get("acct")
        .and_then(Value::as_str)
        .ok_or_else(|| Error::Fetch("the credentials answer has no acct".to_string()))?;
    let avatar = who
        .get("avatar")
        .and_then(Value::as_str)
        .unwrap_or_default();

    Ok((
        token.to_string(),
        ConnectedAccount {
            acct: acct.to_string(),
            avatar: avatar.to_string(),
        },
    ))
}

/// Tell the instance to stop honoring a token: the disconnect button's
/// other half.
pub async fn revoke_token(http: &HttpClient, app: &RegisteredApp, token: &str) -> Result<()> {
    let form = [
        ("client_id".to_string(), app.client_id.clone()),
        ("client_secret".to_string(), app.client_secret.clone()),
        ("token".to_string(), token.to_string()),
    ];
    let url = format!(
        "{scheme}://{server}/oauth/revoke",
        scheme = http.scheme(),
        server = app.server
    );
    let resp = http.post_form(&url, &form, None).await?;
    if resp.status != 200 {
        return Err(Error::Fetch(format!(
            "revoking the token failed with status {}",
            resp.status
        )));
    }
    Ok(())
}

/// The instance's own consent screen, pointed at with everything the dance
/// needs. The scheme is the http client's: https in production, and http
/// when the tests play the instance.
pub fn authorize_url(app: &RegisteredApp, scheme: &str, origin: &str, state: &str) -> String {
    let query = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("client_id", &app.client_id)
        .append_pair("redirect_uri", &format!("{origin}/oauth/callback"))
        .append_pair("response_type", "code")
        .append_pair("scope", SCOPES)
        .append_pair("state", state)
        .finish();
    format!("{scheme}://{}/oauth/authorize?{query}", app.server)
}

/// The public origin the instance redirects back to: what a reverse proxy
/// forwards, or the request's own host when there is no proxy. Derived the
/// same way at registration and at the callback, so the two redirect URIs
/// always agree.
pub fn public_origin(headers: &reqwest::header::HeaderMap) -> String {
    let proto = headers
        .get("x-forwarded-proto")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("http");
    let host = headers
        .get("x-forwarded-host")
        .and_then(|v| v.to_str().ok())
        .or_else(|| headers.get("host").and_then(|v| v.to_str().ok()))
        .unwrap_or("127.0.0.1:8080");
    format!("{proto}://{host}")
}
