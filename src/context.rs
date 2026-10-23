//! Finding the posts around the posts we already have.

use indexmap::IndexSet;
use sqlx::SqlitePool;
use tracing::{debug, error, info, warn};

use crate::api::mastodon::HomeServer;
use crate::api::{client_for, find_post};
use crate::db::{is_private_post, mark_private_post};
use crate::http::HttpClient;
use crate::posts::Post;
use crate::run::CancelToken;
use crate::servers::get_server_info;
use crate::store::{RepliedToot, State};

/// Whether the toot's context may be fetched, recording private toots.
///
/// A post the home server reports as non-public can never have its context
/// fetched, so it is recorded in the database the first time it is seen —
/// and only then logged — and recognised without another word afterwards.
/// A post recorded by an earlier run is skipped outright, whatever this
/// run's copy of it says. Both skips are silent at the default log level;
/// at DEBUG each one leaves a line.
pub async fn toot_context_can_be_fetched(toot: &Post, db: &SqlitePool) -> bool {
    if !toot.is_public {
        match mark_private_post(db, &toot.uri).await {
            Ok(true) => debug!("Cannot fetch context of private toot {}", toot.uri),
            Ok(false) => {
                debug!("Skipping private toot {}: already recorded as private", toot.uri)
            }
            Err(ex) => warn!("Could not record private toot {}: {ex}", toot.uri),
        }
        return false;
    }
    match is_private_post(db, &toot.uri).await {
        Ok(true) => {
            debug!(
                "Skipping private toot {}: recorded as private by an earlier run",
                toot.uri
            );
            false
        }
        Ok(false) => true,
        Err(ex) => {
            warn!("Could not check the private toots for {}: {ex}", toot.uri);
            true
        }
    }
}

/// Get the context toots of the given toots from their original server.
pub async fn get_all_known_context_urls(
    server: &str,
    reply_toots: &[Post],
    http: &HttpClient,
    cancel: &CancelToken,
    state: &mut State,
    db: &SqlitePool,
) -> Vec<String> {
    let mut known_context_urls: IndexSet<String> = IndexSet::new();

    for toot in reply_toots {
        if cancel.is_cancelled() {
            break;
        }
        // a private toot is skipped before its URL is even resolved: its
        // context can never be fetched, and remembering it saves later runs
        // the ask
        if !toot_context_can_be_fetched(toot, db).await {
            continue;
        }
        // a boost is only ever a pointer: the replies are on the original
        let url = toot.original().url.clone();
        let Some(parsed_url) = find_post(&url, &mut state.parsed_urls, &mut state.seen_hosts, http).await
        else {
            continue;
        };
        if state
            .recently_checked_context
            .should_fetch(&toot.uri, toot.created_at)
        {
            state
                .recently_checked_context
                .mark_fetched(&toot.uri, toot.created_at);
            let context = get_toot_context(&parsed_url.0, &parsed_url.1, &url, http, state).await;
            for item in context {
                known_context_urls.insert(item);
            }
        }
    }

    let prefix = format!("https://{server}/");
    let known_context_urls: Vec<String> = known_context_urls
        .into_iter()
        .filter(|url| !url.starts_with(&prefix))
        .collect();
    info!("{}", known_context_found_line(&known_context_urls));

    known_context_urls
}

/// The "Found N known context toots" line: the count, and the URLs of the
/// toots found, comma separated. An empty set gets the bare count.
fn known_context_found_line(urls: &[String]) -> String {
    if urls.is_empty() {
        return "Found 0 known context toots".to_string();
    }
    format!(
        "Found {} known context toots: {}",
        urls.len(),
        urls.join(", ")
    )
}

/// Get the server and ID of the toots the given toots replied to.
pub async fn get_all_replied_toot_server_ids(
    server: &str,
    reply_toots: &[Post],
    http: &HttpClient,
    cancel: &CancelToken,
    state: &mut State,
) -> Vec<RepliedToot> {
    let mut replied = Vec::new();
    for toot in reply_toots {
        if cancel.is_cancelled() {
            break;
        }
        if let Some(entry) = get_replied_toot_server_id(server, toot, http, state).await {
            replied.push(entry);
        }
    }
    replied
}

/// Get the server and ID of the toot the given toot replied to.
pub async fn get_replied_toot_server_id(
    server: &str,
    toot: &Post,
    http: &HttpClient,
    state: &mut State,
) -> Option<RepliedToot> {
    let mentions: Vec<_> = toot
        .mentions
        .iter()
        .filter(|mention| mention.id.as_deref() == toot.in_reply_to_account_id.as_deref())
        .collect();
    if mentions.is_empty() {
        return None;
    }
    let mention = mentions[0];
    // only ever asked of reply toots, which always name the post replied to
    let reply_id = toot.in_reply_to_id.as_ref()?;

    let o_url = format!("https://{server}/@{}/{reply_id}", mention.acct);
    if let Some(cached) = state.replied_toot_server_ids.get(&o_url) {
        // a cache entry that survived a run is JSON, so the PostRef is a list
        return cached.clone();
    }

    let url = http.get_redirect_url(&o_url).await?;

    match find_post(&url, &mut state.parsed_urls, &mut state.seen_hosts, http).await {
        Some(post_ref) => {
            let entry = RepliedToot(url, post_ref);
            state.replied_toot_server_ids.insert(o_url, Some(entry.clone()));
            Some(entry)
        }
        None => {
            // find_post has already logged why the URL could not be parsed
            state.replied_toot_server_ids.insert(o_url, None);
            None
        }
    }
}

/// Get the URLs of the context toots of the given toots.
pub async fn get_all_context_urls(
    server: &str,
    replied_toot_ids: Vec<RepliedToot>,
    http: &HttpClient,
    cancel: &CancelToken,
    state: &mut State,
) -> Vec<String> {
    let mut urls = Vec::new();
    // the filter is the outer server's, not the replied-toot's own: a post on
    // our server never needs fetching from anywhere
    let prefix = format!("https://{server}/");
    for entry in replied_toot_ids {
        if cancel.is_cancelled() {
            break;
        }
        let RepliedToot(url, post_ref) = entry;
        let context = get_toot_context(&post_ref.0, &post_ref.1, &url, http, state).await;
        for context_url in context {
            if !context_url.starts_with(&prefix) {
                urls.push(context_url);
            }
        }
    }
    urls
}

/// Get the URLs of the context toots of the given toot.
pub async fn get_toot_context(
    server: &str,
    toot_id: &str,
    toot_url: &str,
    http: &HttpClient,
    state: &mut State,
) -> Vec<String> {
    let Some(post_server) = get_server_info(server, &mut state.seen_hosts, http).await else {
        error!("server {server} not found for post");
        return Vec::new();
    };

    let Some(client) = client_for(&post_server, http) else {
        return Vec::new();
    };

    client.fetch_context_urls(toot_id, toot_url).await
}

/// Add the given toot URLs to the server.
pub async fn add_context_urls(
    home: &HomeServer,
    context_urls: Vec<String>,
    cancel: &CancelToken,
    state: &mut State,
) {
    let mut count = 0;
    let mut failed = 0;
    for url in context_urls {
        if cancel.is_cancelled() {
            break;
        }
        if !state.seen_urls.contains(&url) {
            if home.resolve(&url).await {
                state.seen_urls.add(&url, None);
                count += 1;
            } else {
                failed += 1;
            }
        }
    }

    info!("Added {count} new context toots (with {failed} failures)");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_known_context_line_lists_the_urls() {
        let urls = vec![
            "https://remote.example/@alice/1".to_string(),
            "https://other.example/@bob/2".to_string(),
        ];
        assert_eq!(
            known_context_found_line(&urls),
            "Found 2 known context toots: https://remote.example/@alice/1, https://other.example/@bob/2"
        );
    }

    #[test]
    fn the_known_context_line_without_toots_is_the_bare_count() {
        assert_eq!(
            known_context_found_line(&[]),
            "Found 0 known context toots"
        );
    }
}
