//! Fetching the posts of accounts we do not follow yet.

use tracing::{debug, error, info, warn};

use crate::api::{client_for, find_post};
use crate::context::{add_context_urls, context_fetchable, known_context_urls};
use crate::db::mark_opted_out_account;
use crate::posts::Post;
use crate::run::Context;
use crate::servers::server_info;
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
            UserSet::KnownFollowings => state.known_followings_mut(),
            UserSet::RecentlyCheckedUsers => state.recently_checked_users_mut(),
        }
    }
}

/// Which of an account's own wishes a backfill goes past: `known`
/// fetches an account that is already known, `opt_out` one that has
/// asked not to be crawled. A run goes past neither — only accounts that
/// are new and have not opted out are fetched — while the web UI's
/// buttons go past the ones they say they do.
#[derive(Clone, Copy, Default)]
pub struct Force {
    /// Fetch the account even though it is already known.
    pub known: bool,
    /// Fetch the account even though it has opted out.
    pub opt_out: bool,
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

/// Fetch one account's posts. With `force_opt_out`, an account that has
/// opted out is fetched anyway — only the data page's button on an
/// opted-out account passes that.
pub async fn user_posts(
    ctx: &mut Context<'_>,
    user: &User,
    target: UserSet,
    force_opt_out: bool,
) -> Option<Vec<Post>> {
    if ctx.config.respect_user_opt_out && !force_opt_out {
        if let Some(reason) = opt_out_reason(user) {
            // the skip costs no requests — it happens before any server is
            // contacted — but the account is recorded so the log line fires
            // the first time only; the account is still re-checked each run,
            // so one that later opts in is fetched after all
            let first_seen = match mark_opted_out_account(ctx.db, &user.acct, reason).await {
                Ok(first_seen) => first_seen,
                Err(ex) => {
                    warn!("Could not record opted-out account {}: {ex}", user.acct);
                    true
                }
            };
            if first_seen {
                info!(
                    "Skipping {}: the account has opted out ({reason})",
                    user.acct
                );
            } else {
                debug!(
                    "Skipping {}: the account has opted out ({reason})",
                    user.acct
                );
            }
            return None;
        }
    }
    let Some(host) = host_of(&user.url) else {
        error!("Error parsing Profile URL {}", user.url);
        // We are adding it as 'known' anyway, because we won't be able to fix this.
        target.get(ctx.state).add(&user.acct, None);
        return None;
    };

    if host == ctx.home.server {
        debug!("{} is a local user. Skip", user.acct);
        target.get(ctx.state).add(&user.acct, None);
        return None;
    }

    let Some(post_server) = server_info(&host, ctx.state.seen_hosts_mut(), ctx.http).await else {
        error!("server {host} not found for post");
        return None;
    };

    let client = client_for(&post_server, ctx.http)?;

    // the server has told us what it runs, so only its own URL shapes are tried
    let Some(username) = client.username_from(&user.url) else {
        error!("Error parsing Profile URL {}", user.url);
        target.get(ctx.state).add(&user.acct, None);
        return None;
    };

    client
        .fetch_user_posts(&username, &user.url, ctx.config.max_posts_per_account)
        .await
}

pub async fn add_post_with_context(ctx: &mut Context<'_>, post: &Post) -> bool {
    if ctx.home.resolve(&post.url).await {
        ctx.state.seen_urls_mut().add(&post.url, None);
        if post.may_have_context() && ctx.config.backfill_with_context {
            // a private post is skipped before its URL is even resolved
            if !context_fetchable(post, ctx.db).await {
                return true;
            }
            if find_post(&post.url, ctx.state, ctx.http).await.is_none() {
                return true;
            }
            let known_context_urls =
                known_context_urls(ctx, std::slice::from_ref(post), "backfill", false).await;
            add_context_urls(ctx, known_context_urls).await;
        }
        return true;
    }

    false
}

/// Add the posts of the given accounts. `force` says which of the
/// accounts' own wishes the fetch goes past: a run passes
/// `Force::default()`, fetching only accounts that are neither known
/// nor opted out; the web UI's buttons pass what they need.
pub async fn add_user_posts(
    ctx: &mut Context<'_>,
    followings: Vec<User>,
    target: UserSet,
    force: Force,
) {
    let home_server = ctx.home.server.clone();
    for user in followings {
        if ctx.check_in().await {
            break;
        }
        if (!ctx.state.all_known_users().contains(&user.acct) || force.known)
            && !user.url.starts_with(&format!("https://{home_server}/"))
        {
            let posts = user_posts(ctx, &user, target, force.opt_out).await;

            if let Some(posts) = posts {
                let mut count = 0;
                let mut failed = 0;
                for post in posts {
                    if ctx.check_in().await {
                        break;
                    }
                    if !post.is_boost() && !ctx.state.seen_urls().contains(&post.url) {
                        let added = add_post_with_context(ctx, &post).await;
                        if added {
                            ctx.state.seen_urls_mut().add(&post.url, None);
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
                    target.get(ctx.state).add(&user.acct, None);
                    ctx.state.all_known_users_mut().add(&user.acct, None);
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
        assert_eq!(
            opt_out_reason(&not_indexable).unwrap(),
            "it is not indexable"
        );

        let mut not_discoverable = user("a", "");
        not_discoverable.discoverable = false;
        assert_eq!(
            opt_out_reason(&not_discoverable).unwrap(),
            "it is not discoverable"
        );
    }
}
