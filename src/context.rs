//! Finding the posts around the posts we already have.

use indexmap::IndexSet;
use tracing::{debug, error, info};

use crate::api::mastodon::HomeServer;
use crate::api::{client_for, find_post};
use crate::http::HttpClient;
use crate::posts::Post;
use crate::servers::get_server_info;
use crate::store::{RepliedToot, State};

pub fn toot_context_can_be_fetched(toot: &Post) -> bool {
    if !toot.is_public {
        debug!("Cannot fetch context of private toot {}", toot.uri);
    }
    toot.is_public
}

/// Get the context toots of the given toots from their original server.
pub async fn get_all_known_context_urls(
    server: &str,
    reply_toots: &[Post],
    http: &HttpClient,
    state: &mut State,
) -> Vec<String> {
    let mut known_context_urls: IndexSet<String> = IndexSet::new();

    for toot in reply_toots {
        // a boost is only ever a pointer: the replies are on the original
        let url = toot.original().url.clone();
        let Some(parsed_url) = find_post(&url, &mut state.parsed_urls, &mut state.seen_hosts, http).await
        else {
            continue;
        };
        if toot_context_can_be_fetched(toot)
            && state
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
    info!("Found {} known context toots", known_context_urls.len());

    known_context_urls
}

/// Get the server and ID of the toots the given toots replied to.
pub async fn get_all_replied_toot_server_ids(
    server: &str,
    reply_toots: &[Post],
    http: &HttpClient,
    state: &mut State,
) -> Vec<RepliedToot> {
    let mut replied = Vec::new();
    for toot in reply_toots {
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
            error!("Error parsing toot URL {url}");
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
    state: &mut State,
) -> Vec<String> {
    let mut urls = Vec::new();
    // the filter is the outer server's, not the replied-toot's own: a post on
    // our server never needs fetching from anywhere
    let prefix = format!("https://{server}/");
    for entry in replied_toot_ids {
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
pub async fn add_context_urls(home: &HomeServer, context_urls: Vec<String>, state: &mut State) {
    let mut count = 0;
    let mut failed = 0;
    for url in context_urls {
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
