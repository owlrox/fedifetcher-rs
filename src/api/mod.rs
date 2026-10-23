//! The fediverse APIs FediFetcher can talk to, and the one function that
//! picks the right one for a server.

pub mod lemmy;
pub mod mastodon;
pub mod misskey;
pub mod paging;
pub mod peertube;

use serde_json::Value;
use tracing::error;

use crate::error::{Error, Result};
use crate::http::HttpClient;
use crate::posts::Post;
use crate::servers::{ApiFlavor, ServerInfo, server_info};
use crate::state::ServerCache;
use crate::store::State;
use crate::urls::{PostRef, host_of};

pub use lemmy::LemmyApi;
pub use mastodon::{HomeServer, MastodonApi};
pub use misskey::MisskeyApi;
pub use peertube::PeerTubeApi;

/// A client for one server, whichever dialect that server speaks.
///
/// Tried in order, so a server claiming several APIs gets the best supported
/// one.
pub enum Client {
    Mastodon(MastodonApi),
    Lemmy(LemmyApi),
    Misskey(MisskeyApi),
    PeerTube(PeerTubeApi),
}

impl Client {
    pub fn flavor(&self) -> ApiFlavor {
        match self {
            Client::Mastodon(_) => ApiFlavor::Mastodon,
            Client::Lemmy(_) => ApiFlavor::Lemmy,
            Client::Misskey(_) => ApiFlavor::Misskey,
            Client::PeerTube(_) => ApiFlavor::PeerTube,
        }
    }

    /// The post's id in one of this server's post URLs, or None if it is not
    /// one.
    pub async fn post_id_from(&self, post_url: &str) -> Option<String> {
        match self {
            Client::Mastodon(client) => client.post_id_from(post_url).await,
            Client::Lemmy(client) => client.post_id_from(post_url),
            Client::Misskey(client) => client.post_id_from(post_url),
            Client::PeerTube(client) => client.post_id_from(post_url),
        }
    }

    /// The name in one of this server's profile URLs, or None if it is not
    /// one.
    ///
    /// Only ever asked of a server we have already identified, so each client
    /// answers for its own URL shapes and no others.
    pub fn username_from(&self, profile_url: &str) -> Option<String> {
        match self {
            Client::Mastodon(client) => client.username_from(profile_url),
            Client::Lemmy(client) => client.username_from(profile_url),
            Client::Misskey(client) => client.username_from(profile_url),
            Client::PeerTube(client) => client.username_from(profile_url),
        }
    }

    /// At most `limit` recent posts by an account, or None if unreadable.
    ///
    /// Servers hand these out a page at a time, so anything beyond the first
    /// page costs another request to a server that is not ours.
    pub async fn fetch_user_posts(
        &self,
        username: &str,
        profile_url: &str,
        limit: i64,
    ) -> Option<Vec<Post>> {
        match self {
            Client::Mastodon(client) => client.fetch_user_posts(username, profile_url, limit).await,
            Client::Lemmy(client) => client.fetch_user_posts(username, profile_url, limit).await,
            Client::Misskey(client) => client.fetch_user_posts(username, profile_url, limit).await,
            Client::PeerTube(client) => client.fetch_user_posts(username, profile_url, limit).await,
        }
    }

    /// URLs of the posts around a post: its ancestors and replies.
    pub async fn fetch_context_urls(&self, post_id: &str, post_url: &str) -> Vec<String> {
        match self {
            Client::Mastodon(client) => client.fetch_context_urls(post_id, post_url).await,
            Client::Lemmy(client) => client.fetch_context_urls(post_id, post_url).await,
            Client::Misskey(client) => client.fetch_context_urls(post_id, post_url).await,
            Client::PeerTube(client) => client.fetch_context_urls(post_id, post_url).await,
        }
    }
}

/// Pick a client that can talk to this server, if we know how.
pub fn client_for(server: &ServerInfo, http: &HttpClient) -> Option<Client> {
    // tried in order, so a server claiming several APIs gets the best
    // supported one
    if server.supports(ApiFlavor::Mastodon) {
        return Some(Client::Mastodon(MastodonApi::new(
            &server.webserver,
            http.clone(),
        )));
    }
    if server.supports(ApiFlavor::Lemmy) {
        return Some(Client::Lemmy(LemmyApi::new(
            &server.webserver,
            http.clone(),
        )));
    }
    if server.supports(ApiFlavor::Misskey) {
        return Some(Client::Misskey(MisskeyApi::new(
            &server.webserver,
            http.clone(),
        )));
    }
    if server.supports(ApiFlavor::PeerTube) {
        return Some(Client::PeerTube(PeerTubeApi::new(
            &server.webserver,
            http.clone(),
        )));
    }

    error!("server api unknown for {}", server.webserver);
    None
}

/// Which server a post URL is on and what it calls the post, remembering it.
///
/// The host is in the URL; how to read the rest of it is not, so the host is
/// asked what software it runs and its client reads the remainder. Answers
/// are kept because the same URL comes round again, and because a Pleroma
/// object URL costs a redirect to resolve.
pub async fn find_post(url: &str, state: &mut State, http: &HttpClient) -> Option<PostRef> {
    if let Some(cached) = state.parsed_urls().get(url) {
        return cached.clone();
    }

    let found = match look_up_post(url, state.seen_hosts_mut(), http).await {
        Ok(found) => Some(found),
        Err(reason) => {
            error!("Error parsing toot URL {url}: {reason}");
            None
        }
    };
    state
        .parsed_urls_mut()
        .insert(url.to_string(), found.clone());
    found
}

/// Read a post URL, or say which step made that impossible: no host, no
/// known server info (with the reason its lookup failed), no supported API,
/// or a URL shape the server's client does not recognize.
async fn look_up_post(
    url: &str,
    seen_hosts: &mut ServerCache,
    http: &HttpClient,
) -> Result<PostRef> {
    let host = host_of(url).ok_or_else(|| Error::Fetch("the URL has no host".to_string()))?;

    let server = server_info(&host, seen_hosts, http).await.ok_or_else(|| {
        let reason = seen_hosts
            .get(&host)
            .and_then(|info| info.get("error"))
            .and_then(Value::as_str);
        match reason {
            Some(reason) => {
                Error::Fetch(format!("server info for {host} is unavailable: {reason}"))
            }
            None => Error::Fetch(format!("server info for {host} is unavailable")),
        }
    })?;
    let client = client_for(&server, http).ok_or_else(|| {
        Error::Fetch(format!(
            "{host} runs {} and no supported API is known for it",
            server.software
        ))
    })?;

    let post_id = client.post_id_from(url).await.ok_or_else(|| {
        Error::Fetch(format!(
            "the URL is not a {} post URL",
            client.flavor().as_str()
        ))
    })?;
    Ok(PostRef::new(host, post_id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::servers::failed_lookup_state;

    fn dummy_http() -> HttpClient {
        HttpClient::new(
            Config {
                server: "home.example".to_string(),
                access_tokens: vec!["t".to_string()],
                ..Default::default()
            },
            crate::db::memory_pool(),
        )
    }

    #[tokio::test]
    async fn a_failed_server_lookup_reports_the_cached_reason() {
        // the host's earlier lookup failed (robots.txt, say); no request is
        // made, and the error says why
        let mut seen_hosts = ServerCache::new();
        seen_hosts.add(
            "nya.social",
            failed_lookup_state(
                "Error getting host node info for nya.social. Exception: Querying https://nya.social/.well-known/nodeinfo prohibited by robots.txt",
            ),
        );
        let http = dummy_http();

        let reason = look_up_post(
            "https://nya.social/notes/81a131a4b49fff235e0b0575",
            &mut seen_hosts,
            &http,
        )
        .await
        .unwrap_err();

        let reason = reason.to_string();
        assert!(reason.contains("prohibited by robots.txt"), "{reason}");
        assert!(reason.contains("nya.social"), "{reason}");
    }

    #[tokio::test]
    async fn a_url_without_a_host_reports_that() {
        let mut seen_hosts = ServerCache::new();
        let http = dummy_http();
        let reason = look_up_post("not a url", &mut seen_hosts, &http)
            .await
            .unwrap_err();
        assert_eq!(reason.to_string(), "the URL has no host");
    }
}
