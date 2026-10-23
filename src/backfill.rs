//! Fetching the posts of accounts we do not follow yet.

use sqlx::SqlitePool;
use tracing::{debug, error, info};

use crate::api::mastodon::HomeServer;
use crate::api::{client_for, find_post};
use crate::config::Config;
use crate::context::{add_context_urls, get_all_known_context_urls, toot_context_can_be_fetched};
use crate::http::HttpClient;
use crate::posts::Post;
use crate::run::CancelToken;
use crate::servers::get_server_info;
use crate::state::TimestampedSet;
use crate::store::State;
use crate::urls::host_of;
use crate::users::User;

/// Which of the state's user sets a backfill run is filling.
///
/// Python passes the set itself alongside the whole state, which Rust's
/// borrow checker will not allow; naming the set keeps the same choice.
#[derive(Clone, Copy)]
pub enum UserSet {
    KnownFollowings,
    RecentlyCheckedUsers,
}

impl UserSet {
    fn get<'a>(&self, state: &'a mut State) -> &'a mut TimestampedSet {
        match self {
            UserSet::KnownFollowings => &mut state.known_followings,
            UserSet::RecentlyCheckedUsers => &mut state.recently_checked_users,
        }
    }
}

pub fn filter_known_users(users: Vec<User>, known_users: &TimestampedSet) -> Vec<User> {
    users
        .into_iter()
        .filter(|user| !known_users.contains(&user.acct))
        .collect()
}

/// Why the account asked, in any of the usual ways to be left alone.
pub fn opt_out_reason(user: &User) -> Option<&'static str> {
    let note = user.note.to_lowercase();
    if note.contains(" nobot") || note.contains("/tags/nobot") {
        return Some("its profile asks not to be crawled (nobot)");
    }
    if !user.indexable {
        return Some("it is not indexable");
    }
    if !user.discoverable {
        return Some("it is not discoverable");
    }
    None
}

pub async fn get_user_posts(
    user: &User,
    target: UserSet,
    server: &str,
    http: &HttpClient,
    config: &Config,
    state: &mut State,
) -> Option<Vec<Post>> {
    if config.respect_user_opt_out {
        if let Some(reason) = opt_out_reason(user) {
            info!("Skipping {}: the account has opted out ({reason})", user.acct);
            return None;
        }
    }
    let Some(host) = host_of(&user.url) else {
        error!("Error parsing Profile URL {}", user.url);
        // We are adding it as 'known' anyway, because we won't be able to fix this.
        target.get(state).add(&user.acct, None);
        return None;
    };

    if host == server {
        debug!("{} is a local user. Skip", user.acct);
        target.get(state).add(&user.acct, None);
        return None;
    }

    let Some(post_server) = get_server_info(&host, &mut state.seen_hosts, http).await else {
        error!("server {host} not found for post");
        return None;
    };

    let Some(client) = client_for(&post_server, http) else {
        return None;
    };

    // the server has told us what it runs, so only its own URL shapes are tried
    let Some(username) = client.username_from(&user.url) else {
        error!("Error parsing Profile URL {}", user.url);
        target.get(state).add(&user.acct, None);
        return None;
    };

    client
        .fetch_user_posts(&username, &user.url, config.max_posts_per_account)
        .await
}

pub async fn add_post_with_context(
    post: &Post,
    home: &HomeServer,
    http: &HttpClient,
    config: &Config,
    cancel: &CancelToken,
    state: &mut State,
    db: &SqlitePool,
) -> bool {
    if home.resolve(&post.url).await {
        state.seen_urls.add(&post.url, None);
        if post.may_have_context() && config.backfill_with_context {
            // a private post is skipped before its URL is even resolved
            if !toot_context_can_be_fetched(post, db).await {
                return true;
            }
            if find_post(&post.url, &mut state.parsed_urls, &mut state.seen_hosts, http)
                .await
                .is_none()
            {
                return true;
            }
            let known_context_urls = get_all_known_context_urls(
                &home.server,
                std::slice::from_ref(post),
                http,
                cancel,
                state,
                db,
            )
            .await;
            add_context_urls(home, known_context_urls, cancel, state).await;
        }
        return true;
    }

    false
}

pub async fn add_user_posts(
    home: &HomeServer,
    followings: Vec<User>,
    target: UserSet,
    http: &HttpClient,
    config: &Config,
    cancel: &CancelToken,
    state: &mut State,
    db: &SqlitePool,
) {
    let home_server = home.server.clone();
    for user in followings {
        if cancel.is_cancelled() {
            break;
        }
        if !state.all_known_users.contains(&user.acct)
            && !user.url.starts_with(&format!("https://{home_server}/"))
        {
            let posts = get_user_posts(&user, target, &home_server, http, config, state).await;

            if let Some(posts) = posts {
                let mut count = 0;
                let mut failed = 0;
                for post in posts {
                    if cancel.is_cancelled() {
                        break;
                    }
                    if !post.is_boost() && !state.seen_urls.contains(&post.url) {
                        let added =
                            add_post_with_context(&post, home, http, config, cancel, state, db)
                                .await;
                        if added {
                            state.seen_urls.add(&post.url, None);
                            count += 1;
                        } else {
                            failed += 1;
                        }
                    }
                }
                info!(
                    "Added {count} posts for user {} with {failed} errors",
                    user.acct
                );
                if failed == 0 {
                    target.get(state).add(&user.acct, None);
                    state.all_known_users.add(&user.acct, None);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user(acct: &str, note: &str) -> User {
        User {
            acct: acct.to_string(),
            url: format!("https://remote.example/@{acct}"),
            id: Some("1".to_string()),
            note: note.to_string(),
            indexable: true,
            discoverable: true,
        }
    }

    #[test]
    fn filter_known_users_drops_the_known_accts() {
        let mut known = TimestampedSet::new();
        known.add("alice", None);
        let users = vec![user("alice", ""), user("bob", "")];
        let filtered = filter_known_users(users, &known);
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].acct, "bob");
    }

    #[test]
    fn opt_out_reads_the_usual_signs() {
        // ' nobot' needs the space in front, so a bare #nobot does not count
        assert_eq!(
            opt_out_reason(&user("a", "i am a bot, nobot")).unwrap(),
            "its profile asks not to be crawled (nobot)"
        );
        assert!(opt_out_reason(&user("a", "check /tags/nobot please")).is_some());
        assert_eq!(opt_out_reason(&user("a", "#nobot")), None);
        assert_eq!(opt_out_reason(&user("a", "just a person")), None);
        assert_eq!(opt_out_reason(&user("a", "")), None);

        let mut not_indexable = user("a", "");
        not_indexable.indexable = false;
        assert_eq!(opt_out_reason(&not_indexable).unwrap(), "it is not indexable");

        let mut not_discoverable = user("a", "");
        not_discoverable.discoverable = false;
        assert_eq!(
            opt_out_reason(&not_discoverable).unwrap(),
            "it is not discoverable"
        );
    }
}
