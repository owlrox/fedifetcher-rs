//! Manual, at-will fetches, triggered from the web UI's data page: one
//! post's context, one account's posts, one host's server info, or one URL's
//! resolve. Each runs in the background, logs what it did, and updates the
//! same database tables a run would.

use tracing::{error, info, warn};

use crate::api::mastodon::HomeServer;
use crate::backfill::{add_user_posts, Force, UserSet};
use crate::context::{add_context_urls, known_context_urls};
use crate::db;
use crate::http::HttpClient;
use crate::posts::Post;
use crate::run::Context;
use crate::servers::{server_info, ApiFlavor, ServerInfo};
use crate::store::State;
use crate::timeutil;
use crate::users::User;

/// Fetch one post's context now: resolve the post on our server, then pull
/// its context from the server hosting it and resolve that too. `uri` is the
/// post's uri, as the trigger_posts or context_cache table spells it.
pub async fn force_post_context(ctx: &mut Context<'_>, uri: &str) {
    info!("Manual fetch: context for post {uri}");

    // what the database knows about the post: its URL and its age
    let known = match db::trigger_post(ctx.db, uri).await {
        Ok(known) => known,
        Err(ex) => {
            warn!("Manual fetch: could not read the trigger posts for {uri}: {ex}");
            None
        }
    };
    let url = known
        .as_ref()
        .map(|row| row.url.clone())
        .unwrap_or_else(|| uri.to_string());
    let created_at = known
        .as_ref()
        .and_then(|row| row.created_at.as_deref())
        .and_then(timeutil::parse)
        .or_else(|| {
            ctx.state
                .recently_checked_context()
                .get(uri)
                .map(|entry| entry.created_at)
        })
        .unwrap_or_else(timeutil::now);
    let replies_count = known.as_ref().and_then(|row| row.replies_count);

    match db::is_private_post(ctx.db, uri).await {
        Ok(true) => {
            info!("Manual fetch: {uri} is recorded as private; its context cannot be fetched");
            return;
        }
        Ok(false) => {}
        Err(ex) => warn!("Manual fetch: could not check the private toots for {uri}: {ex}"),
    }

    let post = Post {
        url: url.clone(),
        uri: uri.to_string(),
        created_at,
        is_public: true,
        reblog: None,
        in_reply_to_id: None,
        in_reply_to_account_id: None,
        reply_count: replies_count,
        account: None,
        mentions: Vec::new(),
    };

    // pull the post itself into our server, unless it is already ours
    let home_prefix = format!("https://{}/", ctx.config.server);
    if !url.starts_with(&home_prefix) && ctx.home.resolve(&url).await {
        ctx.state.seen_urls_mut().add(&url, None);
    }

    // then its context, ignoring how recently it was last checked
    let context_urls = known_context_urls(ctx, std::slice::from_ref(&post), "manual", true).await;
    add_context_urls(ctx.home, context_urls, &ctx.cancel, ctx.state).await;
}

/// Backfill one account's posts now, even when it is already known. `acct`
/// is `name@host`; a local account has nothing to backfill. With
/// `force_opt_out`, an account recorded as opted out is fetched anyway —
/// the data page's button on the opted_out_accounts table passes that.
pub async fn force_user_posts(
    ctx: &mut Context<'_>,
    acct: &str,
    target: UserSet,
    force_opt_out: bool,
) {
    info!("Manual fetch: backfill posts for {acct}");

    let Some((name, host)) = acct.split_once('@') else {
        info!("Manual fetch: {acct} is a local user; there is nothing to backfill");
        return;
    };
    if name.is_empty() || host.is_empty() {
        info!("Manual fetch: {acct} is not a remote account; there is nothing to backfill");
        return;
    }
    match db::opted_out_account_reason(ctx.db, acct).await {
        Ok(Some(reason)) if !force_opt_out => {
            info!("Manual fetch: not backfilling {acct}: the account has opted out ({reason})");
            return;
        }
        Ok(Some(reason)) => {
            info!("Manual fetch: backfilling {acct} anyway: the account has opted out ({reason})");
        }
        Ok(None) => {}
        Err(ex) => warn!("Manual fetch: could not check the opted-out accounts for {acct}: {ex}"),
    }

    // the profile URL shape the account's server reads, so its client can
    // parse the name out of it
    let Some(server) = server_info(host, ctx.state.seen_hosts_mut(), ctx.http).await else {
        error!("Manual fetch: server info for {host} is unavailable; cannot backfill {acct}");
        return;
    };
    let user = User::new(acct, profile_url_for(&server, host, name));
    add_user_posts(
        ctx,
        vec![user],
        target,
        Force {
            known: true,
            opt_out: force_opt_out,
        },
    )
    .await;
}

/// The profile URL for `name` on `host`, in the shape the server's own API
/// client reads. The flavors are tried in the order `client_for` tries
/// them, so the URL matches the client that will read it.
fn profile_url_for(server: &ServerInfo, host: &str, name: &str) -> String {
    for flavor in [
        ApiFlavor::Mastodon,
        ApiFlavor::Lemmy,
        ApiFlavor::Misskey,
        ApiFlavor::PeerTube,
    ] {
        if server.supports(flavor) {
            return match flavor {
                ApiFlavor::Lemmy => format!("https://{host}/u/{name}"),
                ApiFlavor::PeerTube => format!("https://{host}/accounts/{name}"),
                // mastodon and misskey both address profiles as /@name
                _ => format!("https://{host}/@{name}"),
            };
        }
    }
    format!("https://{host}/@{name}")
}

/// Re-check one host's server info now, ignoring the cached copy.
pub async fn force_host_info(host: &str, http: &HttpClient, state: &mut State) {
    info!("Manual fetch: re-checking server info for {host}");
    // forget the cached copy, so the lookup runs again
    state.seen_hosts_mut().remove(host);
    match server_info(host, state.seen_hosts_mut(), http).await {
        Some(info) => info!(
            "Manual fetch: {host} runs {} {}",
            info.software, info.version
        ),
        None => {
            let reason = state
                .seen_hosts()
                .get(host)
                .and_then(|entry| entry.get("error"))
                .and_then(serde_json::Value::as_str)
                .unwrap_or("unknown reason");
            warn!("Manual fetch: could not re-check {host}: {reason}");
        }
    }
}

/// Ask our server to resolve one URL again, pulling it in if it is new.
pub async fn force_resolve_url(home: &HomeServer, state: &mut State, url: &str) {
    info!("Manual fetch: re-resolving {url} on {}", home.server);
    if home.resolve(url).await {
        state.seen_urls_mut().add(url, None);
    } else {
        warn!("Manual fetch: {} did not resolve {url}", home.server);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn server(software: &str) -> ServerInfo {
        ServerInfo::from_nodeinfo(
            "remote.example",
            &json!({
                "software": {"name": software, "version": "1"},
                "protocols": ["activitypub"],
            }),
        )
        .unwrap()
    }

    #[test]
    fn the_profile_url_matches_the_servers_flavor() {
        assert_eq!(
            profile_url_for(&server("mastodon"), "remote.example", "alice"),
            "https://remote.example/@alice"
        );
        assert_eq!(
            profile_url_for(&server("sharkey"), "remote.example", "alice"),
            "https://remote.example/@alice"
        );
        assert_eq!(
            profile_url_for(&server("lemmy"), "remote.example", "alice"),
            "https://remote.example/u/alice"
        );
        assert_eq!(
            profile_url_for(&server("peertube"), "remote.example", "alice"),
            "https://remote.example/accounts/alice"
        );
    }
}
