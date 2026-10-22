//! What FediFetcher remembers between runs.
//!
//! The serialized shapes match the Python original exactly, so a state
//! directory can be shared between the two implementations.

use indexmap::IndexMap;
use serde_json::{Map, Value};
use time::{Duration, OffsetDateTime};

use crate::timeutil;

/// An insertion-ordered set that remembers when each item was first added.
///
/// Only the first add of an item counts: re-adding something keeps the
/// original timestamp, so ages reflect first sight rather than last.
#[derive(Clone, Debug, Default)]
pub struct TimestampedSet {
    map: IndexMap<String, OffsetDateTime>,
}

impl TimestampedSet {
    pub fn new() -> Self {
        Self::default()
    }

    /// A set of plain lines, each stamped with the current time.
    pub fn from_lines(lines: &[String]) -> Self {
        let mut set = TimestampedSet::new();
        for line in lines {
            set.add(line, None);
        }
        set
    }

    pub fn add(&mut self, item: &str, when: Option<OffsetDateTime>) {
        self.map
            .entry(item.to_string())
            .or_insert_with(|| when.unwrap_or_else(timeutil::now));
    }

    pub fn get(&self, item: &str) -> Option<OffsetDateTime> {
        self.map.get(item).copied()
    }

    pub fn contains(&self, item: &str) -> bool {
        self.map.contains_key(item)
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = &String> {
        self.map.keys()
    }

    pub fn update<I: IntoIterator<Item = String>>(&mut self, items: I) {
        for item in items {
            self.add(&item, None);
        }
    }

    /// Drop everything added longer ago than `age`, and report how many went.
    pub fn expire_older_than(&mut self, age: Duration) -> usize {
        let now = timeutil::now();
        let before = self.map.len();
        self.map.retain(|_, when| now - *when <= age);
        before - self.map.len()
    }

    /// `{item: timestamp}`, the shape the Python original writes.
    pub fn to_json(&self) -> Value {
        let mut map = Map::new();
        for (item, when) in &self.map {
            map.insert(item.clone(), Value::String(timeutil::fmt_state_ts(*when)));
        }
        Value::Object(map)
    }

    pub fn from_json(value: &Value) -> Self {
        let mut set = TimestampedSet::new();
        if let Value::Object(map) = value {
            for (item, when) in map {
                let Some(when) = when.as_str().and_then(timeutil::parse) else {
                    continue;
                };
                set.add(item, Some(when));
            }
        }
        set
    }
}

/// What we know about each host we have looked up, keyed by hostname.
///
/// The value is the info object as the Python original writes it, with
/// `last_checked` kept as a timestamp string.
#[derive(Clone, Debug, Default)]
pub struct ServerCache {
    map: IndexMap<String, Value>,
}

impl ServerCache {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add(&mut self, key: &str, info: Value) {
        self.map.insert(key.to_string(), info);
    }

    pub fn get(&self, key: &str) -> Option<&Value> {
        self.map.get(key)
    }

    pub fn contains(&self, key: &str) -> bool {
        self.map.contains_key(key)
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = (&String, &Value)> {
        self.map.iter()
    }

    /// Drop stale entries, and report how many went.
    ///
    /// Entries recorded before PeerTube support existed are dropped whatever
    /// their age, because they cannot say whether the host speaks that API.
    pub fn expire(&mut self, max_age: Duration, failure_max_age: Duration) -> usize {
        let now = timeutil::now();
        let before = self.map.len();
        self.map.retain(|_, info| {
            let Some(info) = info.as_object() else {
                return false;
            };
            if !info.contains_key("peertubeApiSupport") {
                return false;
            }
            let Some(last_checked) = info.get("last_checked") else {
                return true;
            };
            let Some(last_checked) = last_checked.as_str().and_then(timeutil::parse) else {
                return true;
            };
            let age = now - last_checked;
            if age > max_age {
                return false;
            }
            if info.get("info") == Some(&Value::Null) && age > failure_max_age {
                return false;
            }
            true
        });
        before - self.map.len()
    }

    pub fn to_json(&self) -> Value {
        let mut map = Map::new();
        for (host, info) in &self.map {
            map.insert(host.clone(), info.clone());
        }
        Value::Object(map)
    }

    pub fn from_json(value: &Value) -> Self {
        let mut cache = ServerCache::new();
        if let Value::Object(map) = value {
            for (host, info) in map {
                cache.add(host, info.clone());
            }
        }
        cache
    }
}

/// When we last fetched the replies to a post, and when the post was made.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ContextEntry {
    pub last_seen: OffsetDateTime,
    pub created_at: OffsetDateTime,
}

/// Remembers when we last fetched the replies to a post.
///
/// Fresh posts gain replies quickly and old ones rarely, so how long we wait
/// before looking again depends on how old the post is.
#[derive(Clone, Debug, Default)]
pub struct ContextCache {
    entries: IndexMap<String, ContextEntry>,
}

impl ContextCache {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn should_fetch(&self, uri: &str, _created_at: OffsetDateTime) -> bool {
        let Some(entry) = self.entries.get(uri) else {
            return true;
        };

        let now = timeutil::now();
        let since_last_seen = (now - entry.last_seen).whole_seconds();
        let age = (now - entry.created_at).whole_seconds();

        if age <= 60 * 60 && since_last_seen >= 60 {
            // For the first hour: allow refetching once per minute
            return true;
        }
        if age <= 24 * 60 * 60 && since_last_seen >= 10 * 60 {
            // For the rest of the first day: once every 10 minutes
            return true;
        }
        if since_last_seen >= 60 * 60 {
            // After that: hourly
            return true;
        }
        false
    }

    pub fn mark_fetched(&mut self, uri: &str, created_at: OffsetDateTime) {
        self.entries.insert(
            uri.to_string(),
            ContextEntry {
                last_seen: timeutil::now(),
                created_at,
            },
        );
    }

    /// Forget posts we have not seen in a while, so this cannot grow forever.
    pub fn expire_older_than(&mut self, age: Duration) -> usize {
        let now = timeutil::now();
        let before = self.entries.len();
        self.entries.retain(|_, entry| now - entry.last_seen <= age);
        before - self.entries.len()
    }

    pub fn contains(&self, uri: &str) -> bool {
        self.entries.contains_key(uri)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// `{uri: {"lastSeen": ..., "created_at": ...}}`, as the Python original
    /// writes it (note the camelCase key).
    pub fn to_json(&self) -> Value {
        let mut map = Map::new();
        for (uri, entry) in &self.entries {
            let mut inner = Map::new();
            inner.insert(
                "lastSeen".to_string(),
                Value::String(timeutil::fmt_state_ts(entry.last_seen)),
            );
            inner.insert(
                "created_at".to_string(),
                Value::String(timeutil::fmt_state_ts(entry.created_at)),
            );
            map.insert(uri.clone(), Value::Object(inner));
        }
        Value::Object(map)
    }

    pub fn from_json(value: &Value) -> Self {
        let mut cache = ContextCache::new();
        if let Value::Object(map) = value {
            for (uri, entry) in map {
                let (Some(last_seen), Some(created_at)) = (
                    entry.get("lastSeen").and_then(Value::as_str).and_then(timeutil::parse),
                    entry.get("created_at").and_then(Value::as_str).and_then(timeutil::parse),
                ) else {
                    continue;
                };
                cache.entries.insert(
                    uri.clone(),
                    ContextEntry {
                        last_seen,
                        created_at,
                    },
                );
            }
        }
        cache
    }
}

#[cfg(test)]
mod tests {
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
    fn timestamped_set_json_round_trips() {
        let mut set = TimestampedSet::new();
        set.add("acct1", Some(datetime!(2026-10-22 01:23:45.123456 UTC)));
        set.add("acct2", Some(datetime!(2026-10-22 02:00:00 UTC)));
        let json = set.to_json();
        let back = TimestampedSet::from_json(&json);
        assert_eq!(back.get("acct1"), Some(datetime!(2026-10-22 01:23:45.123456 UTC)));
        assert_eq!(back.get("acct2"), Some(datetime!(2026-10-22 02:00:00 UTC)));
        // the serialized shape is an object of item -> timestamp string
        assert_eq!(
            json["acct1"],
            Value::String(timeutil::fmt_state_ts(datetime!(2026-10-22 01:23:45.123456 UTC)))
        );
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
    fn context_cache_json_round_trips() {
        let mut cache = ContextCache::new();
        cache.mark_fetched("uri", datetime!(2026-10-22 01:23:45 UTC));
        let json = cache.to_json();
        assert!(json["uri"].get("lastSeen").is_some());
        assert!(json["uri"].get("created_at").is_some());
        let back = ContextCache::from_json(&json);
        assert_eq!(back.len(), 1);
        assert!(back.contains("uri"));
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
        cache.add("failed", serde_json::json!({
            "info": null, "last_checked": timeutil::fmt_state_ts(now - Duration::hours(2)),
            "peertubeApiSupport": false,
        }));
        // failed lookup, recent: kept
        cache.add("failed-recent", serde_json::json!({
            "info": null, "last_checked": timeutil::fmt_state_ts(now - Duration::minutes(5)),
            "peertubeApiSupport": false,
        }));
        // entry from before peertube support existed: dropped whatever its age
        cache.add("legacy", serde_json::json!({
            "webserver": "h", "software": "mastodon", "version": "4",
            "last_checked": timeutil::fmt_state_ts(now),
        }));

        let dropped = cache.expire(Duration::days(30), Duration::hours(1));
        assert_eq!(dropped, 3);
        assert!(cache.contains("fresh"));
        assert!(cache.contains("failed-recent"));
        assert!(!cache.contains("stale"));
        assert!(!cache.contains("failed"));
        assert!(!cache.contains("legacy"));
    }
}
