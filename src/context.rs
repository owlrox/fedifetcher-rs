//! Finding the posts around the posts we already have.

use indexmap::IndexSet;
use sqlx::SqlitePool;
use time::{Duration, OffsetDateTime};
use tracing::{debug, error, info, warn};

use crate::api::mastodon::HomeServer;
use crate::api::{client_for, find_post};
use crate::db::{self, is_private_post, mark_private_post, TriggerPost};
use crate::http::HttpClient;
use crate::posts::Post;
use crate::run::{CancelToken, Context};
use crate::servers::server_info;
use crate::store::{RepliedToot, State};
use crate::timeutil;

/// The minimum time between handling an already-known context post again.
/// The home server has the post from the first time it was resolved, so
/// re-resolving it would only chatter at the instance hosting it. A day
/// keeps that to a recovery path — the state files can drop a URL from
/// `seen_urls` — while still healing within a day.
const KNOWN_CONTEXT_RECHECK: Duration = Duration::days(1);

/// Whether the toot's context may be fetched, recording private toots.
///
/// A post the home server reports as non-public can never have its context
/// fetched, so it is recorded in the database the first time it is seen —
/// and only then logged — and recognised without another word afterwards.
/// A post recorded by an earlier run is skipped outright, whatever this
/// run's copy of it says. Both skips are silent at the default log level;
/// at DEBUG each one leaves a line.
pub async fn context_fetchable(toot: &Post, db: &SqlitePool) -> bool {
    if !toot.is_public {
        match mark_private_post(db, &toot.uri).await {
            Ok(true) => debug!("Cannot fetch context of private toot {}", toot.uri),
            Ok(false) => {
                debug!(
                    "Skipping private toot {}: already recorded as private",
                    toot.uri
                )
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

/// Get the context toots of the given toots from their original server,
/// recording them in the database. Only the posts that need handling are
/// returned: new ones, and already-known ones last seen longer ago than
/// KNOWN_CONTEXT_RECHECK — the rest the home server already has, and
/// re-resolving them would only chatter at the instance hosting them.
///
/// The toots given are recorded as trigger posts under `source`, so the data
/// page can show what a run scanned — and fetch one post's context again on
/// demand. `force` skips the recency check, for a manual fetch.
pub async fn known_context_urls(
    ctx: &mut Context<'_>,
    reply_toots: &[Post],
    source: &str,
    force: bool,
) -> Vec<String> {
    let mut known_context_urls: IndexSet<String> = IndexSet::new();
    // what became of the toots the scan was given, so the summary line can
    // say why it found what it found
    let mut scan = ContextScan {
        total: reply_toots.len(),
        ..Default::default()
    };
    // every toot the scan is given, as a trigger post, recorded after the
    // loop in one write
    let mut triggers: Vec<TriggerPost> = Vec::with_capacity(reply_toots.len());

    for toot in reply_toots {
        if ctx.cancel.is_cancelled() {
            break;
        }
        triggers.push(TriggerPost::from_post(toot, source));
        // a private toot is skipped before its URL is even resolved: its
        // context can never be fetched, and remembering it saves later runs
        // the ask
        if !context_fetchable(toot, ctx.db).await {
            scan.private += 1;
            continue;
        }
        // a boost is only ever a pointer: the replies are on the original
        let url = toot.original().url.clone();
        let Some(parsed_url) = find_post(
            &url,
            &mut ctx.state.parsed_urls,
            &mut ctx.state.seen_hosts,
            ctx.http,
        )
        .await
        else {
            scan.unresolved += 1;
            continue;
        };
        if force
            || ctx
                .state
                .recently_checked_context
                .should_fetch(&toot.uri, toot.created_at)
        {
            ctx.state
                .recently_checked_context
                .mark_fetched(&toot.uri, toot.created_at);
            scan.fetched += 1;
            let context = toot_context_urls(
                &parsed_url.server,
                &parsed_url.post_id,
                &url,
                ctx.http,
                ctx.state,
            )
            .await;
            for item in context {
                known_context_urls.insert(item);
            }
        } else {
            scan.recently_checked += 1;
        }
    }

    // remember the posts this scan encountered, so the data page can list
    // them and fetch one again on demand
    match db::record_trigger_posts(ctx.db, &triggers, db::TRIGGER_POSTS_KEPT).await {
        Ok(fresh) => debug!("Recorded {} trigger posts ({fresh} new)", triggers.len()),
        Err(ex) => warn!("Could not record the trigger posts in the database: {ex}"),
    }

    let prefix = format!("https://{}/", ctx.config.server);
    let found: Vec<String> = known_context_urls
        .into_iter()
        .filter(|url| {
            if url.starts_with(&prefix) {
                scan.local += 1;
                false
            } else {
                true
            }
        })
        .collect();

    // record what the run found, and keep only what needs handling
    let (to_handle, known) = match db::record_known_context_posts(ctx.db, &found).await {
        Ok(previous) => {
            let (to_handle, new, already_known) =
                partition_known_context(&found, previous, timeutil::now());
            (to_handle, Some((new, already_known)))
        }
        Err(ex) => {
            warn!("Could not record the known context toots in the database: {ex}");
            (found.clone(), None)
        }
    };

    info!("{}", known_context_found_line(&found, &scan, known));

    to_handle
}

/// Split the found context posts into what needs handling and what the
/// database already knew about: a post is handled when it is new, or when
/// it was last seen longer ago than KNOWN_CONTEXT_RECHECK. Returns the
/// posts to handle, and the counts of new and already-known posts.
fn partition_known_context(
    found: &[String],
    previous_last_seen: Vec<Option<String>>,
    now: OffsetDateTime,
) -> (Vec<String>, usize, usize) {
    let mut to_handle = Vec::new();
    let mut new = 0;
    for (url, last_seen) in found.iter().zip(previous_last_seen) {
        match last_seen {
            None => {
                new += 1;
                to_handle.push(url.clone());
            }
            Some(last_seen) => {
                let recently_seen = timeutil::parse(&last_seen)
                    .map(|seen| now - seen < KNOWN_CONTEXT_RECHECK)
                    // an unreadable timestamp is handled again, not trusted
                    .unwrap_or(false);
                if !recently_seen {
                    to_handle.push(url.clone());
                }
            }
        }
    }
    (to_handle, new, found.len() - new)
}

/// What became of the toots a context scan was given, so the summary line
/// can say why it found what it found.
#[derive(Default)]
struct ContextScan {
    /// The toots the scan was given.
    total: usize,
    /// Skipped as private, or recorded as private by an earlier run.
    private: usize,
    /// Skipped because their original URL could not be resolved.
    unresolved: usize,
    /// Skipped because their context was fetched recently.
    recently_checked: usize,
    /// Whose context was fetched.
    fetched: usize,
    /// Context URLs dropped because they are on our own server.
    local: usize,
}

/// The "Found N known context toots" line: the count, and the URLs of the
/// toots found, comma separated. An empty set gets the bare count. What the
/// scan did with its toots follows, so a bare "Found 0" says why; URLs on
/// our own server are counted separately, as they are dropped, not found.
/// `known` is the (new, already known) split of the database record, so a
/// run that found nothing new says so.
fn known_context_found_line(
    urls: &[String],
    scan: &ContextScan,
    known: Option<(usize, usize)>,
) -> String {
    let found = if urls.is_empty() {
        "Found 0 known context toots".to_string()
    } else {
        format!(
            "Found {} known context toots: {}",
            urls.len(),
            urls.join(", ")
        )
    };
    let mut parts = vec![format!(
        "from {} toots: {} private, {} unresolved, {} already checked recently, {} fetched",
        scan.total, scan.private, scan.unresolved, scan.recently_checked, scan.fetched
    )];
    if scan.local > 0 {
        parts.push(format!("{} on our server", scan.local));
    }
    if let Some((new, already_known)) = known {
        if !urls.is_empty() {
            parts.push(format!("{new} new, {already_known} already known"));
        }
    }
    format!("{found} ({})", parts.join("; "))
}

/// Get the server and ID of the toots the given toots replied to.
pub async fn replied_toot_server_ids(
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
        if let Some(entry) = replied_toot_server_id(server, toot, http, state).await {
            replied.push(entry);
        }
    }
    replied
}

/// Get the server and ID of the toot the given toot replied to.
pub async fn replied_toot_server_id(
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
            let entry = RepliedToot { url, post_ref };
            state
                .replied_toot_server_ids
                .insert(o_url, Some(entry.clone()));
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
pub async fn context_urls(
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
        let RepliedToot { url, post_ref } = entry;
        let context =
            toot_context_urls(&post_ref.server, &post_ref.post_id, &url, http, state).await;
        for context_url in context {
            if !context_url.starts_with(&prefix) {
                urls.push(context_url);
            }
        }
    }
    urls
}

/// Get the URLs of the context toots of the given toot.
pub async fn toot_context_urls(
    server: &str,
    toot_id: &str,
    toot_url: &str,
    http: &HttpClient,
    state: &mut State,
) -> Vec<String> {
    let Some(post_server) = server_info(server, &mut state.seen_hosts, http).await else {
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
    let total = context_urls.len();
    let mut count = 0;
    let mut failed = 0;
    let mut already_seen = 0;
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
        } else {
            already_seen += 1;
        }
    }

    info!("{}", added_context_line(count, failed, total, already_seen));
}

/// The "Added N new context toots" line: what was added, what failed, and
/// what became of the URLs given — already-seen URLs are why a run can add
/// nothing at all.
fn added_context_line(count: usize, failed: usize, total: usize, already_seen: usize) -> String {
    format!(
        "Added {count} new context toots (with {failed} failures) from {total} context urls ({already_seen} already seen)"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_known_context_line_lists_the_urls_and_the_scan() {
        let urls = vec![
            "https://remote.example/@alice/1".to_string(),
            "https://other.example/@bob/2".to_string(),
        ];
        let scan = ContextScan {
            total: 5,
            recently_checked: 2,
            fetched: 3,
            ..Default::default()
        };
        assert_eq!(
            known_context_found_line(&urls, &scan, Some((1, 1))),
            "Found 2 known context toots: https://remote.example/@alice/1, https://other.example/@bob/2 (from 5 toots: 0 private, 0 unresolved, 2 already checked recently, 3 fetched; 1 new, 1 already known)"
        );
    }

    #[test]
    fn the_known_context_line_without_toots_says_why() {
        let scan = ContextScan {
            total: 3,
            private: 1,
            unresolved: 1,
            recently_checked: 1,
            ..Default::default()
        };
        assert_eq!(
            known_context_found_line(&[], &scan, Some((0, 0))),
            "Found 0 known context toots (from 3 toots: 1 private, 1 unresolved, 1 already checked recently, 0 fetched)"
        );
        // no toots given at all, and no database record to report
        assert_eq!(
            known_context_found_line(&[], &ContextScan::default(), None),
            "Found 0 known context toots (from 0 toots: 0 private, 0 unresolved, 0 already checked recently, 0 fetched)"
        );
    }

    #[test]
    fn the_known_context_line_counts_urls_on_our_own_server() {
        let scan = ContextScan {
            total: 1,
            fetched: 1,
            local: 2,
            ..Default::default()
        };
        assert_eq!(
            known_context_found_line(&[], &scan, None),
            "Found 0 known context toots (from 1 toots: 0 private, 0 unresolved, 0 already checked recently, 1 fetched; 2 on our server)"
        );
    }

    #[test]
    fn new_context_posts_are_all_handled() {
        let found = vec![
            "https://remote.example/@a/1".to_string(),
            "https://remote.example/@a/2".to_string(),
        ];
        let (to_handle, new, known) =
            partition_known_context(&found, vec![None, None], timeutil::now());
        assert_eq!(to_handle, found);
        assert_eq!((new, known), (2, 0));
    }

    #[test]
    fn recently_known_context_posts_are_not_handled_again() {
        let found = vec!["https://remote.example/@a/1".to_string()];
        let recent = timeutil::fmt_state_ts(timeutil::now() - Duration::hours(1));
        let (to_handle, new, known) =
            partition_known_context(&found, vec![Some(recent)], timeutil::now());
        assert!(
            to_handle.is_empty(),
            "seen within the re-check interval, so the home server already has it"
        );
        assert_eq!((new, known), (0, 1));
    }

    #[test]
    fn stale_context_posts_are_handled_again() {
        let found = vec!["https://remote.example/@a/1".to_string()];
        let stale = timeutil::fmt_state_ts(timeutil::now() - Duration::days(2));
        let (to_handle, new, known) =
            partition_known_context(&found, vec![Some(stale)], timeutil::now());
        assert_eq!(to_handle, found, "last seen beyond the re-check interval");
        assert_eq!((new, known), (0, 1));

        // an unreadable timestamp is handled again rather than trusted
        let (to_handle, _, _) = partition_known_context(
            &found,
            vec![Some("not a timestamp".to_string())],
            timeutil::now(),
        );
        assert_eq!(to_handle, found);
    }

    #[test]
    fn the_added_context_line_says_what_became_of_the_urls() {
        assert_eq!(
            added_context_line(2, 1, 5, 2),
            "Added 2 new context toots (with 1 failures) from 5 context urls (2 already seen)"
        );
        // nothing given at all
        assert_eq!(
            added_context_line(0, 0, 0, 0),
            "Added 0 new context toots (with 0 failures) from 0 context urls (0 already seen)"
        );
        // everything was already seen
        assert_eq!(
            added_context_line(0, 0, 3, 3),
            "Added 0 new context toots (with 0 failures) from 3 context urls (3 already seen)"
        );
    }
}
