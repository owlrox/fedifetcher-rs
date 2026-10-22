//! The fediverse APIs FediFetcher can talk to, and the one function that
//! picks the right one for a server.

pub mod lemmy;
pub mod mastodon;
pub mod misskey;
pub mod paging;
pub mod peertube;

use indexmap::IndexMap;
use tracing::error;

use crate::http::HttpClient;
use crate::posts::Post;
use crate::servers::{ApiFlavour, ServerInfo, get_server_info};
use crate::state::ServerCache;
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
    pub fn flavour(&self) -> ApiFlavour {
        match self {
            Client::Mastodon(_) => ApiFlavour::Mastodon,
            Client::Lemmy(_) => ApiFlavour::Lemmy,
            Client::Misskey(_) => ApiFlavour::Misskey,
            Client::PeerTube(_) => ApiFlavour::PeerTube,
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
    if server.supports(ApiFlavour::Mastodon) {
        return Some(Client::Mastodon(MastodonApi::new(&server.webserver, http.clone())));
    }
    if server.supports(ApiFlavour::Lemmy) {
        return Some(Client::Lemmy(LemmyApi::new(&server.webserver, http.clone())));
    }
    if server.supports(ApiFlavour::Misskey) {
        return Some(Client::Misskey(MisskeyApi::new(&server.webserver, http.clone())));
    }
    if server.supports(ApiFlavour::PeerTube) {
        return Some(Client::PeerTube(PeerTubeApi::new(&server.webserver, http.clone())));
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
pub async fn find_post(
    url: &str,
    parsed_urls: &mut IndexMap<String, Option<PostRef>>,
    seen_hosts: &mut ServerCache,
    http: &HttpClient,
) -> Option<PostRef> {
    if let Some(cached) = parsed_urls.get(url) {
        return cached.clone();
    }

    let found = look_up_post(url, seen_hosts, http).await;
    if found.is_none() {
        error!("Error parsing toot URL {url}");
    }
    parsed_urls.insert(url.to_string(), found.clone());
    found
}

async fn look_up_post(
    url: &str,
    seen_hosts: &mut ServerCache,
    http: &HttpClient,
) -> Option<PostRef> {
    let host = host_of(url)?;

    let server = get_server_info(&host, seen_hosts, http).await?;
    let client = client_for(&server, http)?;

    let post_id = client.post_id_from(url).await?;
    Some(PostRef(host, post_id))
}
