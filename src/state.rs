//! What FediFetcher remembers between runs, in memory.
//!
//! The database in the state directory is what persists between runs (see
//! `db::load_state` and `db::save_state`); these are the shapes a run works
//! with.

use indexmap::IndexMap;
use serde_json::Value;
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

    /// Every item with the time it was first added, in insertion order.
    pub fn entries(&self) -> impl Iterator<Item = (&String, OffsetDateTime)> {
        self.map.iter().map(|(item, when)| (item, *when))
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

    /// Forget a host, so the next lookup asks it again.
    pub fn remove(&mut self, key: &str) -> Option<Value> {
        self.map.shift_remove(key)
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
        self.record(uri, timeutil::now(), created_at);
    }

    /// Record a fetch with the times it actually happened — the load path
    /// knows both timestamps.
    pub fn record(&mut self, uri: &str, last_seen: OffsetDateTime, created_at: OffsetDateTime) {
        self.entries.insert(
            uri.to_string(),
            ContextEntry {
                last_seen,
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

    /// When a post was last fetched, and when it was made.
    pub fn get(&self, uri: &str) -> Option<ContextEntry> {
        self.entries.get(uri).copied()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Every entry, in insertion order.
    pub fn entries(&self) -> impl Iterator<Item = (&String, ContextEntry)> {
        self.entries.iter().map(|(uri, entry)| (uri, *entry))
    }
}

#[cfg(test)]
mod tests;
