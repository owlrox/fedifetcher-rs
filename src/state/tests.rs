//! The tests for `state.rs`. Kept in a child module so they see the
//! private items; the parent holds only the `mod tests;` pointer.

use super::*;
use time::macros::datetime;

#[test]
fn timestamped_set_keeps_first_add() {
    let mut set = TimestampedSet::new();
    let first = datetime!(2026-10-22 00:00:00 UTC);
    let second = datetime!(2026-10-22 01:00:00 UTC);
    set.add("a", Some(first));
    set.add("a", Some(second));
    assert_eq!(set.get("a"), Some(first));
    assert_eq!(set.len(), 1);
}

#[test]
fn timestamped_set_expires() {
    let mut set = TimestampedSet::new();
    let old = timeutil::now() - Duration::days(10);
    let fresh = timeutil::now();
    set.add("old", Some(old));
    set.add("fresh", Some(fresh));
    assert_eq!(set.expire_older_than(Duration::days(7)), 1);
    assert!(!set.contains("old"));
    assert!(set.contains("fresh"));
}

#[test]
fn context_cache_fetch_policy() {
    let mut cache = ContextCache::new();
    let created = timeutil::now() - Duration::minutes(30);
    // never seen: fetch
    assert!(cache.should_fetch("uri1", created));
    cache.mark_fetched("uri1", created);
    // seen just now, post is 30 minutes old: not yet (needs 10 min for the first day)
    assert!(!cache.should_fetch("uri1", created));

    // a fresh post (10 minutes old) may be refetched after a minute
    let mut cache = ContextCache::new();
    let created = timeutil::now() - Duration::minutes(10);
    cache.mark_fetched("uri2", created);
    assert!(!cache.should_fetch("uri2", created));
    cache.entries.get_mut("uri2").unwrap().last_seen = timeutil::now() - Duration::minutes(2);
    assert!(cache.should_fetch("uri2", created));

    // an old post (2 days old) may be refetched after an hour
    let mut cache = ContextCache::new();
    let created = timeutil::now() - Duration::days(2);
    cache.mark_fetched("uri3", created);
    assert!(!cache.should_fetch("uri3", created));
    cache.entries.get_mut("uri3").unwrap().last_seen = timeutil::now() - Duration::hours(2);
    assert!(cache.should_fetch("uri3", created));
}

#[test]
fn server_cache_remove_forgets_a_host() {
    let mut cache = ServerCache::new();
    cache.add("a.example", serde_json::json!({"x": 1}));
    assert!(cache.remove("a.example").is_some());
    assert!(!cache.contains("a.example"));
    assert!(
        cache.remove("a.example").is_none(),
        "removing twice finds nothing"
    );
}

#[test]
fn context_cache_get_reads_an_entry() {
    let mut cache = ContextCache::new();
    let created = timeutil::now() - Duration::minutes(30);
    assert!(cache.get("uri").is_none());
    cache.mark_fetched("uri", created);
    let entry = cache.get("uri").unwrap();
    assert_eq!(entry.created_at, created);
}

#[test]
fn server_cache_expiry() {
    let now = timeutil::now();
    let info = |last_checked: OffsetDateTime, extra: &str| {
        serde_json::json!({
            "webserver": "h", "software": "mastodon", "version": "4",
            "last_checked": timeutil::fmt_state_ts(last_checked),
            "peertubeApiSupport": false, "mastodonApiSupport": true,
            "extra": extra,
        })
    };
    let mut cache = ServerCache::new();
    // fresh entry: kept
    cache.add("fresh", info(now, "x"));
    // too old: dropped
    cache.add("stale", info(now - Duration::days(31), "x"));
    // failed lookup, older than an hour: dropped
    cache.add(
        "failed",
        serde_json::json!({
            "info": null, "last_checked": timeutil::fmt_state_ts(now - Duration::hours(2)),
            "peertubeApiSupport": false,
        }),
    );
    // failed lookup, recent: kept
    cache.add(
        "failed-recent",
        serde_json::json!({
            "info": null, "last_checked": timeutil::fmt_state_ts(now - Duration::minutes(5)),
            "peertubeApiSupport": false,
        }),
    );
    // entry from before peertube support existed: dropped whatever its age
    cache.add(
        "legacy",
        serde_json::json!({
            "webserver": "h", "software": "mastodon", "version": "4",
            "last_checked": timeutil::fmt_state_ts(now),
        }),
    );

    let dropped = cache.expire(Duration::days(30), Duration::hours(1));
    assert_eq!(dropped, 3);
    assert!(cache.contains("fresh"));
    assert!(cache.contains("failed-recent"));
    assert!(!cache.contains("stale"));
    assert!(!cache.contains("failed"));
    assert!(!cache.contains("legacy"));
}

#[test]
fn update_adds_every_item_untimestamped() {
    let mut set = TimestampedSet::new();
    set.update(["a".to_string(), "b".to_string()]);
    assert_eq!(set.len(), 2);
    assert!(set.get("a").is_some() && set.get("b").is_some());
}

#[test]
fn expire_drops_odd_entries_and_keeps_uncheckable_ones() {
    let mut cache = ServerCache::new();
    // not an object at all: dropped whatever its age
    cache.add("garbage", serde_json::json!("just a string"));
    // no last_checked: kept, there is nothing to age
    cache.add(
        "no-timestamp",
        serde_json::json!({"peertubeApiSupport": false}),
    );
    // an unparseable last_checked: kept rather than guessed about
    cache.add(
        "bad-timestamp",
        serde_json::json!({"peertubeApiSupport": false, "last_checked": "not a date"}),
    );
    let dropped = cache.expire(Duration::days(30), Duration::hours(1));
    assert_eq!(dropped, 1, "only the non-object goes");
    assert!(cache.contains("no-timestamp"));
    assert!(cache.contains("bad-timestamp"));
}

#[test]
fn an_old_post_is_refetched_at_most_hourly() {
    let now = timeutil::now();
    let mut cache = ContextCache::new();
    assert!(cache.is_empty());
    // fetched two days ago, last seen two hours ago: the hourly cadence
    cache.entries.insert(
        "https://example.com/post/1".to_string(),
        ContextEntry {
            last_seen: now - Duration::hours(2),
            created_at: now - Duration::days(2),
        },
    );
    assert!(!cache.is_empty());
    assert!(cache.should_fetch("https://example.com/post/1", now));
}
