//! The things a run does: pulling context for posts, and backfilling
//! accounts, each only when its setting asks for it.

use time::{Duration, OffsetDateTime};
use tracing::{debug, error, info};

use crate::api::mastodon::HomeServer;
use crate::backfill::{add_user_posts, filter_known_users, UserSet};
use crate::config::Config;
use crate::context::{add_context_urls, context_urls, known_context_urls, replied_toot_server_ids};
use crate::error::Result;
use crate::posts::Post;
use crate::run::{CancelToken, Context};
use crate::store::State;
use crate::timeutil;
use crate::users::User;

/// Get all replies to other users by the given users in the last day.
pub async fn reply_toots(
    home: &HomeServer,
    user_ids: &[String],
    reply_interval_hours: i64,
    cancel: &CancelToken,
    state: &mut State,
) -> Vec<Post> {
    // a post's date is UTC, so the window has to be measured in UTC too:
    // local time here quietly shortened the interval east of UTC and
    // lengthened it west
    let replies_since = timeutil::now() - Duration::hours(reply_interval_hours);
    let mut reply_toots = Vec::new();
    for user_id in user_ids {
        if cancel.is_canceled() {
            break;
        }
        reply_toots.extend(reply_toots_for_user(user_id, home, replies_since, state).await);
    }
    info!("Found {} reply toots", reply_toots.len());
    reply_toots
}

/// Get replies by the user to other users since the given date.
async fn reply_toots_for_user(
    user_id: &str,
    home: &HomeServer,
    reply_since: OffsetDateTime,
    state: &mut State,
) -> Vec<Post> {
    let statuses = match home.account_statuses(user_id).await {
        Ok(statuses) => statuses,
        Err(ex) => {
            error!(
                "Error getting replies for user {user_id} on server {}: {ex}",
                home.server
            );
            return Vec::new();
        }
    };

    let toots: Vec<Post> = statuses
        .into_iter()
        .filter(|toot| {
            toot.in_reply_to_id.is_some()
                && !state.seen_urls().contains(&toot.url)
                && toot.created_at > reply_since
        })
        .collect();
    for toot in &toots {
        debug!("Found reply toot: {}", toot.url);
    }
    toots
}

async fn fetch_timeline_context(ctx: &mut Context<'_>, timeline_posts: Vec<Post>, source: &str) {
    let known_context_urls = known_context_urls(ctx, &timeline_posts, source, false).await;
    add_context_urls(ctx.home, known_context_urls, &ctx.cancel, ctx.state).await;

    // Backfill any post authors, and any mentioned users
    if ctx.config.backfill_mentioned_users {
        let mut mentioned_users: Vec<User> = Vec::new();
        let cut_off = timeutil::now_local() - Duration::minutes(60);
        for toot in &timeline_posts {
            let mut these_users: Vec<User> = Vec::new();
            if mentioned_users.len() < 10
                || (toot.created_at > cut_off && mentioned_users.len() < 30)
            {
                these_users.extend(accounts_named_by(toot));
                if let Some(reblog) = &toot.reblog {
                    these_users.extend(accounts_named_by(reblog));
                }
            }
            for user in these_users {
                if !mentioned_users.contains(&user)
                    && !ctx.state.all_known_users().contains(&user.acct)
                {
                    mentioned_users.push(user);
                }
            }
        }

        let new_users = filter_known_users(mentioned_users, ctx.state.all_known_users());
        add_user_posts(ctx, new_users, UserSet::RecentlyCheckedUsers, false).await;
    }
}

/// Who wrote a post, and everyone it mentions.
fn accounts_named_by(toot: &Post) -> Vec<User> {
    let mut users = Vec::new();
    if let Some(account) = &toot.account {
        users.push(account.clone());
    }
    users.extend(toot.mentions.iter().cloned());
    users
}

/// Pull replies to posts in the API key owner's lists, and backfill their
/// members.
async fn fetch_from_lists(ctx: &mut Context<'_>) -> Result<()> {
    let lists = ctx.home.lists().await?;
    info!("Getting context for {} lists", lists.len());
    for user_list in lists {
        if ctx.config.max_list_length > 0 {
            let timeline_toots = ctx
                .home
                .list_timeline(&user_list.id, ctx.config.max_list_length)
                .await?;
            info!(
                "Found {} toots in list {}",
                timeline_toots.len(),
                user_list.title
            );
            fetch_timeline_context(ctx, timeline_toots, "lists").await;
        }

        if ctx.config.max_list_accounts > 0 {
            let accounts = ctx
                .home
                .list_accounts(&user_list.id, ctx.config.max_list_accounts)
                .await?;
            info!(
                "Found {} accounts in list {}",
                accounts.len(),
                user_list.title
            );
            add_user_posts(ctx, accounts, UserSet::RecentlyCheckedUsers, false).await;
        }
    }
    Ok(())
}

/// Pull the context of toots our users replied to, from the original server.
async fn fetch_reply_context(ctx: &mut Context<'_>) -> Result<()> {
    let user_ids = ctx
        .home
        .active_user_ids(ctx.config.reply_interval_in_hours as f64)
        .await?;
    let reply_toots = reply_toots(
        ctx.home,
        &user_ids,
        ctx.config.reply_interval_in_hours,
        &ctx.cancel,
        ctx.state,
    )
    .await;
    let known_context_urls = known_context_urls(ctx, &reply_toots, "replies", false).await;
    ctx.state.seen_urls_mut().update(known_context_urls);
    let replied_toot_ids = replied_toot_server_ids(
        &ctx.config.server,
        &reply_toots,
        ctx.http,
        &ctx.cancel,
        ctx.state,
    )
    .await;
    let context_urls = context_urls(
        &ctx.config.server,
        replied_toot_ids,
        ctx.http,
        &ctx.cancel,
        ctx.state,
    )
    .await;
    add_context_urls(ctx.home, context_urls, &ctx.cancel, ctx.state).await;
    Ok(())
}

/// Pull replies to posts on the API key owner's home timeline.
async fn fetch_home_timeline(ctx: &mut Context<'_>) -> Result<()> {
    info!("Getting context for home timeline");
    let timeline_toots = ctx.home.timeline(ctx.config.home_timeline_length).await?;
    fetch_timeline_context(ctx, timeline_toots, "home timeline").await;
    Ok(())
}

/// Backfill posts of accounts our user has recently followed.
async fn backfill_followings(ctx: &mut Context<'_>) -> Result<()> {
    info!(
        "Getting posts from last {} followings",
        ctx.config.max_followings
    );
    let user_id = ctx.home.user_id(config_user(ctx.config)).await?;
    let followings = ctx
        .home
        .following(&user_id, ctx.config.max_followings)
        .await?;
    let total = followings.len();
    let new_followings = filter_known_users(followings, ctx.state.all_known_users());
    let new = new_followings.len();
    info!("Got {total} followings, {new} of which are new");
    add_user_posts(ctx, new_followings, UserSet::KnownFollowings, false).await;
    Ok(())
}

/// Backfill posts of accounts that recently followed our user.
async fn backfill_followers(ctx: &mut Context<'_>) -> Result<()> {
    info!(
        "Getting posts from last {} followers",
        ctx.config.max_followers
    );
    let user_id = ctx.home.user_id(config_user(ctx.config)).await?;
    let followers = ctx
        .home
        .followers(&user_id, ctx.config.max_followers)
        .await?;
    let total = followers.len();
    let new_followers = filter_known_users(followers, ctx.state.all_known_users());
    let new = new_followers.len();
    info!("Got {total} followers, {new} of which are new");
    add_user_posts(ctx, new_followers, UserSet::RecentlyCheckedUsers, false).await;
    Ok(())
}

/// Backfill posts of accounts with a pending follow request.
async fn backfill_follow_requests(ctx: &mut Context<'_>) -> Result<()> {
    info!(
        "Getting posts from last {} follow requests",
        ctx.config.max_follow_requests
    );
    let follow_requests = ctx
        .home
        .follow_requests(ctx.config.max_follow_requests)
        .await?;
    let total = follow_requests.len();
    let new_requests = filter_known_users(follow_requests, ctx.state.all_known_users());
    let new = new_requests.len();
    info!("Got {total} follow_requests, {new} of which are new");
    add_user_posts(ctx, new_requests, UserSet::RecentlyCheckedUsers, false).await;
    Ok(())
}

/// Backfill posts of accounts appearing in recent notifications.
async fn backfill_from_notifications(ctx: &mut Context<'_>) -> Result<()> {
    info!(
        "Getting notifications for last {} hours",
        ctx.config.from_notifications
    );
    let since = timeutil::now_local() - Duration::hours(ctx.config.from_notifications);
    let accounts = ctx.home.notification_accounts(since).await?;
    let total = accounts.len();
    let new_accounts = filter_known_users(accounts, ctx.state.all_known_users());
    let new = new_accounts.len();
    info!("Found {total} users in notifications, {new} of which are new");
    add_user_posts(ctx, new_accounts, UserSet::RecentlyCheckedUsers, false).await;
    Ok(())
}

/// Pull replies to the API key owner's bookmarks.
async fn fetch_bookmark_context(ctx: &mut Context<'_>) -> Result<()> {
    info!(
        "Pulling replies to the last {} bookmarks",
        ctx.config.max_bookmarks
    );
    let bookmarks = ctx.home.bookmarks(ctx.config.max_bookmarks).await?;
    pull_context_for(ctx, bookmarks, "bookmarks").await;
    Ok(())
}

/// Pull replies to the API key owner's favorites.
async fn fetch_favorite_context(ctx: &mut Context<'_>) -> Result<()> {
    info!(
        "Pulling replies to the last {} favorites",
        ctx.config.max_favorites
    );
    let favorites = ctx.home.favorites(ctx.config.max_favorites).await?;
    pull_context_for(ctx, favorites, "favorites").await;
    Ok(())
}

async fn pull_context_for(ctx: &mut Context<'_>, posts: Vec<Post>, source: &str) {
    let known_context_urls = known_context_urls(ctx, &posts, source, false).await;
    add_context_urls(ctx.home, known_context_urls, &ctx.cancel, ctx.state).await;
}

/// The user whose followings and followers are backfilled, if a specific one
/// was configured.
fn config_user(config: &Config) -> Option<&str> {
    if config.user.is_empty() {
        None
    } else {
        Some(&config.user)
    }
}

/// Run each task whose setting asks for it, in the Python original's order.
///
/// A canceled run stops between tasks: whatever finished is kept, and the
/// run reports that it was stopped.
pub async fn run_enabled_tasks(ctx: &mut Context<'_>) -> Result<()> {
    if ctx.config.from_lists {
        fetch_from_lists(ctx).await?;
    }
    if ctx.cancel.is_canceled() {
        return Ok(());
    }
    if ctx.config.reply_interval_in_hours > 0 {
        fetch_reply_context(ctx).await?;
    }
    if ctx.cancel.is_canceled() {
        return Ok(());
    }
    if ctx.config.home_timeline_length > 0 {
        fetch_home_timeline(ctx).await?;
    }
    if ctx.cancel.is_canceled() {
        return Ok(());
    }
    if ctx.config.max_followings > 0 {
        backfill_followings(ctx).await?;
    }
    if ctx.cancel.is_canceled() {
        return Ok(());
    }
    if ctx.config.max_followers > 0 {
        backfill_followers(ctx).await?;
    }
    if ctx.cancel.is_canceled() {
        return Ok(());
    }
    if ctx.config.max_follow_requests > 0 {
        backfill_follow_requests(ctx).await?;
    }
    if ctx.cancel.is_canceled() {
        return Ok(());
    }
    if ctx.config.from_notifications > 0 {
        backfill_from_notifications(ctx).await?;
    }
    if ctx.cancel.is_canceled() {
        return Ok(());
    }
    if ctx.config.max_bookmarks > 0 {
        fetch_bookmark_context(ctx).await?;
    }
    if ctx.cancel.is_canceled() {
        return Ok(());
    }
    if ctx.config.max_favorites > 0 {
        fetch_favorite_context(ctx).await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accounts_named_by_collects_author_and_mentions() {
        let author = User::new("alice", "https://remote.example/@alice");
        let mentioned = User::new("bob@remote.example", "https://remote.example/@bob");
        let post = Post {
            url: "https://remote.example/@alice/1".to_string(),
            uri: "https://remote.example/@alice/1".to_string(),
            created_at: timeutil::now(),
            is_public: true,
            reblog: None,
            in_reply_to_id: None,
            in_reply_to_account_id: None,
            reply_count: None,
            account: Some(author),
            mentions: vec![mentioned],
        };
        let named = accounts_named_by(&post);
        assert_eq!(named.len(), 2);
        assert_eq!(named[0].acct, "alice");
        assert_eq!(named[1].acct, "bob@remote.example");
    }

    #[test]
    fn config_user_is_none_when_unset() {
        let config = Config::default();
        assert_eq!(config_user(&config), None);
        let config = Config {
            user: "alice".to_string(),
            ..Config::default()
        };
        assert_eq!(config_user(&config), Some("alice"));
    }
}
