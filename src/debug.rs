//! The debug tab's actions: direct calls to our own server's API, for
//! checking a configuration without running anything.
//!
//! Each action takes the server's base URL rather than a host name, the way
//! `get_paginated` does, so tests can point them at a mock server. None of
//! them touch the state on disk, and robots.txt does not apply to our own
//! server.

use serde_json::Value;

use crate::api::mastodon::{masked, report_mastodon_error};
use crate::error::{Error, Result};
use crate::http::HttpClient;

/// What a debug action found, as the page shows it.
#[derive(Clone, Debug)]
pub enum DebugOutcome {
    /// The server answered, and the token was accepted.
    Connected { instance: Value, account: Value },
    /// An account lookup succeeded.
    User { acct: String, account: Value },
    /// An account's recent posts, with the account itself for context.
    Posts {
        acct: String,
        account: Value,
        posts: Vec<Value>,
    },
    /// Something went wrong; the message says what.
    Failed { message: String },
}

/// The connection test: what the server says it is, and whether it accepts
/// the token. Both halves have to work for a run to.
pub async fn test_connection(base: &str, token: &str, http: &HttpClient) -> Result<(Value, Value)> {
    let instance = instance_info(base, http).await?;
    let account = verify_credentials(base, token, http).await?;
    Ok((instance, account))
}

/// GET /api/v1/instance: what the server says it is. Needs no token.
pub async fn instance_info(base: &str, http: &HttpClient) -> Result<Value> {
    let url = format!("{base}/api/v1/instance");
    let resp = http.ignoring_robots().get(&url, None).await?;
    if resp.status != 200 {
        return Err(Error(format!(
            "Error getting instance info from {base}. Status code: {}",
            resp.status
        )));
    }
    resp.json()
}

/// GET /api/v1/accounts/verify_credentials: is the token valid?
pub async fn verify_credentials(base: &str, token: &str, http: &HttpClient) -> Result<Value> {
    let url = format!("{base}/api/v1/accounts/verify_credentials");
    let resp = http.authenticated(token).get(&url, None).await?;
    if resp.status != 200 {
        return Err(report_mastodon_error(
            &format!("Error verifying credentials on server {base}"),
            resp.status,
            "read:accounts",
            Some(&masked(token)),
        ));
    }
    resp.json()
}

/// GET /api/v1/accounts/lookup: one account, as our server reports it.
pub async fn lookup_account(
    base: &str,
    acct: &str,
    token: &str,
    http: &HttpClient,
) -> Result<Value> {
    let acct = acct.trim().trim_start_matches('@');
    let url = format!("{base}/api/v1/accounts/lookup?acct={}", encode(acct));
    let resp = http.authenticated(token).get(&url, None).await?;
    if resp.status == 200 {
        return resp.json();
    }
    if resp.status == 404 {
        return Err(Error(format!(
            "User {acct} was not found on server {base}."
        )));
    }
    Err(report_mastodon_error(
        &format!("Error looking up user {acct} on server {base}"),
        resp.status,
        "read:accounts",
        Some(&masked(token)),
    ))
}

/// One account's recent posts: the account, and its raw statuses.
pub async fn account_posts(
    base: &str,
    acct: &str,
    limit: i64,
    token: &str,
    http: &HttpClient,
) -> Result<(Value, Vec<Value>)> {
    let account = lookup_account(base, acct, token, http).await?;
    let id = account
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| Error("account has no id".to_string()))?;
    let url = format!("{base}/api/v1/accounts/{id}/statuses?exclude_replies=false&limit={limit}");
    let resp = http.authenticated(token).get(&url, None).await?;
    if resp.status != 200 {
        return Err(report_mastodon_error(
            &format!("Error getting posts for user {acct} on server {base}"),
            resp.status,
            "read:statuses",
            Some(&masked(token)),
        ));
    }
    let posts = match resp.json()? {
        Value::Array(items) => items,
        _ => Vec::new(),
    };
    Ok((account, posts))
}

/// Percent-encode a query parameter's value.
fn encode(text: &str) -> String {
    url::form_urlencoded::byte_serialize(text.as_bytes()).collect()
}
