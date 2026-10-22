//! Reading and writing the state directory, and the lock that keeps two runs
//! from overlapping.
//!
//! The file layout and formats match the Python original: two plain-text
//! collections (`seen_urls`, `known_followings`), four JSON collections, a
//! version file, and a lock file whose content is the time the run started.

use std::path::PathBuf;

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use time::Duration;
use tracing::{debug, warn};

use crate::config::Config;
use crate::error::LockedError;
use crate::state::{ContextCache, ServerCache, TimestampedSet};
use crate::timeutil;
use crate::urls::PostRef;

/// The format the state files are written in; a directory without a version
/// file predates versioning and is read as version 1.
pub const STATE_VERSION: u32 = 1;

/// How many entries of the append-only collections to carry over between runs.
pub const MAX_ENTRIES: usize = 100_000;

pub const CONTEXT_MAX_AGE: Duration = Duration::days(7);
pub const ROBOTS_MAX_AGE: Duration = Duration::days(1);
pub const SERVER_FAILURE_MAX_AGE: Duration = Duration::hours(1);

/// A post that was replied to: the URL we followed, and where it landed.
///
/// Serializes as `[url, [server, post_id]]`, the shape the Python original
/// writes into `replied_toot_server_ids`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RepliedToot(pub String, pub PostRef);

impl Serialize for RepliedToot {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        (&self.0, &self.1).serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for RepliedToot {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let (url, post) = <(String, PostRef)>::deserialize(deserializer)?;
        Ok(RepliedToot(url, post))
    }
}

/// Everything FediFetcher remembers between runs, plus this run's scratch
/// space.
#[derive(Default)]
pub struct State {
    pub seen_urls: TimestampedSet,
    pub known_followings: TimestampedSet,
    pub recently_checked_users: TimestampedSet,
    pub seen_hosts: ServerCache,
    pub recently_checked_context: ContextCache,
    pub replied_toot_server_ids: IndexMap<String, Option<RepliedToot>>,

    /// Rebuilt on load, and not written back out.
    pub all_known_users: TimestampedSet,
    pub parsed_urls: IndexMap<String, Option<PostRef>>,
}

/// Reads and writes the state directory.
pub struct StateStore {
    config: Config,
}

impl StateStore {
    pub fn new(config: Config) -> Self {
        StateStore { config }
    }

    pub fn version_file(&self) -> PathBuf {
        self.config.state_dir.join("state_version")
    }

    pub fn stored_version(&self) -> u32 {
        std::fs::read_to_string(self.version_file())
            .ok()
            .and_then(|text| text.trim().parse::<u32>().ok())
            .unwrap_or(1)
    }

    pub fn load(&self) -> State {
        let config = &self.config;
        let version = self.stored_version();
        if version > STATE_VERSION {
            warn!(
                "State directory is version {version}, but this FediFetcher understands version {STATE_VERSION}. Reading it anyway."
            );
        }

        let mut state = State {
            seen_urls: TimestampedSet::from_lines(&read_lines(&config.seen_urls_file())),
            known_followings: TimestampedSet::from_lines(&read_lines(&config.known_followings_file())),
            recently_checked_users: TimestampedSet::from_json(
                &read_json(&config.recently_checked_users_file()).unwrap_or(Value::Object(Default::default())),
            ),
            seen_hosts: ServerCache::from_json(
                &read_json(&config.seen_hosts_file()).unwrap_or(Value::Object(Default::default())),
            ),
            recently_checked_context: ContextCache::from_json(
                &read_json(&config.recently_checked_contexts_file()).unwrap_or(Value::Object(Default::default())),
            ),
            replied_toot_server_ids: read_json(&config.replied_toot_server_ids_file())
                .and_then(|value| serde_json::from_value(value).ok())
                .unwrap_or_default(),
            ..Default::default()
        };

        state
            .recently_checked_users
            .expire_older_than(Duration::hours(config.remember_users_for_hours));
        state.seen_hosts.expire(
            Duration::days(config.remember_hosts_for_days),
            SERVER_FAILURE_MAX_AGE,
        );
        state
            .recently_checked_context
            .expire_older_than(CONTEXT_MAX_AGE);

        for user in state.known_followings.iter().cloned().collect::<Vec<_>>() {
            state.all_known_users.add(&user, None);
        }
        for user in state.recently_checked_users.iter().cloned().collect::<Vec<_>>() {
            state.all_known_users.add(&user, None);
        }
        state
    }

    pub fn save(&self, state: &State) {
        let config = &self.config;
        std::fs::create_dir_all(&config.state_dir).ok();

        write(
            &config.known_followings_file(),
            &tail_lines(state.known_followings.iter(), MAX_ENTRIES).join("\n"),
        );
        write(
            &config.seen_urls_file(),
            &tail_lines(state.seen_urls.iter(), MAX_ENTRIES).join("\n"),
        );
        let replied: IndexMap<String, Option<RepliedToot>> = state
            .replied_toot_server_ids
            .iter()
            .rev()
            .take(MAX_ENTRIES)
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        write(
            &config.replied_toot_server_ids_file(),
            &serde_json::to_string(&replied).unwrap_or_else(|_| "{}".to_string()),
        );
        write(
            &config.recently_checked_users_file(),
            &serde_json::to_string(&state.recently_checked_users.to_json()).unwrap_or_else(|_| "{}".to_string()),
        );
        write(
            &config.seen_hosts_file(),
            &serde_json::to_string(&state.seen_hosts.to_json()).unwrap_or_else(|_| "{}".to_string()),
        );
        write(
            &config.recently_checked_contexts_file(),
            &serde_json::to_string(&state.recently_checked_context.to_json()).unwrap_or_else(|_| "{}".to_string()),
        );
        write(&self.version_file(), &STATE_VERSION.to_string());
    }

    /// Load state, and write it back even if the run fails part way through.
    pub fn session(&self) -> Session {
        Session {
            store: StateStore {
                config: self.config.clone(),
            },
            state: Some(self.load()),
        }
    }
}

/// A loaded state that is written back when dropped.
pub struct Session {
    store: StateStore,
    state: Option<State>,
}

impl Session {
    pub fn state(&mut self) -> &mut State {
        self.state.as_mut().expect("session state already taken")
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        if let Some(state) = self.state.take() {
            self.store.save(&state);
        }
    }
}

/// Hold the lock file for the duration, so two runs cannot overlap.
pub fn lock(config: &Config) -> Result<LockGuard, LockedError> {
    let path = config.lock_path();

    if path.exists() {
        debug!("Lock file exists at {}", path.display());
        let lock_time = std::fs::read_to_string(&path)
            .ok()
            .and_then(|text| timeutil::parse(&text))
            .ok_or_else(|| LockedError("Cannot read logfile age - aborting.".to_string()))?;

        let age = timeutil::now() - lock_time;
        if age < Duration::hours(config.lock_hours) {
            return Err(LockedError(format!(
                "Lock file age is {} - below --lock-hours={} provided.",
                timeutil::fmt_timedelta(age),
                config.lock_hours
            )));
        }

        std::fs::remove_file(&path).ok();
        debug!("Lock file has expired. Removed lock file.");
    }

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    std::fs::write(&path, timeutil::fmt_lock_ts(timeutil::now()))
        .map_err(|ex| LockedError(format!("Cannot write lock file {}: {ex}", path.display())))?;
    Ok(LockGuard { path })
}

/// The held lock; removed when dropped.
#[derive(Debug)]
pub struct LockGuard {
    path: PathBuf,
}

impl Drop for LockGuard {
    fn drop(&mut self) {
        std::fs::remove_file(&self.path).ok();
    }
}

fn read_lines(path: &PathBuf) -> Vec<String> {
    std::fs::read_to_string(path)
        .map(|text| text.lines().map(str::to_string).collect())
        .unwrap_or_default()
}

fn read_json(path: &PathBuf) -> Option<Value> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

fn write(path: &PathBuf, text: &str) {
    std::fs::write(path, text).ok();
}

fn tail_lines<'a>(items: impl Iterator<Item = &'a String>, max: usize) -> Vec<&'a str> {
    let all: Vec<&'a str> = items.map(String::as_str).collect();
    let start = all.len().saturating_sub(max);
    all[start..].to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn test_config(dir: &std::path::Path) -> Config {
        Config {
            server: "home.example".to_string(),
            access_tokens: vec!["token".to_string()],
            state_dir: dir.to_path_buf(),
            ..Default::default()
        }
    }

    fn temp_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ff-store-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn state_round_trips_through_disk() {
        let dir = temp_dir();
        let store = StateStore::new(test_config(&dir));

        let mut session = store.session();
        let state = session.state();
        state.seen_urls.add("https://remote.example/@a/1", None);
        state.known_followings.add("b@remote.example", None);
        state.recently_checked_users.add("c@remote.example", None);
        state
            .replied_toot_server_ids
            .insert("https://home.example/@b/2".to_string(), Some(RepliedToot(
                "https://remote.example/@a/1".to_string(),
                PostRef::new("remote.example", "1"),
            )));
        state.replied_toot_server_ids.insert("https://home.example/@b/3".to_string(), None);
        state.seen_hosts.add("remote.example", json!({
            "webserver": "remote.example", "software": "mastodon", "version": "4.3",
            "last_checked": timeutil::fmt_state_ts(timeutil::now()),
            "mastodonApiSupport": true, "peertubeApiSupport": false,
        }));
        state.recently_checked_context.mark_fetched("uri", timeutil::now());
        drop(session);

        // the files exist with the expected names
        for name in [
            "seen_urls",
            "known_followings",
            "recently_checked_users",
            "seen_hosts",
            "recent_context",
            "replied_toot_server_ids",
            "state_version",
        ] {
            assert!(dir.join(name).exists(), "{name} missing");
        }
        assert_eq!(std::fs::read_to_string(dir.join("state_version")).unwrap(), "1");

        let loaded = store.load();
        assert!(loaded.seen_urls.contains("https://remote.example/@a/1"));
        assert!(loaded.known_followings.contains("b@remote.example"));
        assert!(loaded.recently_checked_users.contains("c@remote.example"));
        assert!(loaded.seen_hosts.contains("remote.example"));
        assert!(loaded.recently_checked_context.contains("uri"));
        assert_eq!(
            loaded.replied_toot_server_ids.get("https://home.example/@b/2"),
            Some(&Some(RepliedToot(
                "https://remote.example/@a/1".to_string(),
                PostRef::new("remote.example", "1")
            )))
        );
        assert_eq!(
            loaded.replied_toot_server_ids.get("https://home.example/@b/3"),
            Some(&None)
        );
        // all_known_users is rebuilt from followings and recently checked users
        assert!(loaded.all_known_users.contains("b@remote.example"));
        assert!(loaded.all_known_users.contains("c@remote.example"));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn lock_blocks_a_second_run_until_it_expires() {
        let dir = temp_dir();
        let config = test_config(&dir);

        let guard = lock(&config).unwrap();
        assert!(config.lock_path().exists());
        let err = lock(&config).unwrap_err();
        assert!(err.0.contains("below --lock-hours=24"), "{}", err.0);
        drop(guard);
        assert!(!config.lock_path().exists());

        // an expired lock is removed and re-acquired
        std::fs::write(
            config.lock_path(),
            timeutil::fmt_lock_ts(timeutil::now() - Duration::hours(48)),
        )
        .unwrap();
        let guard = lock(&config).unwrap();
        drop(guard);
        assert!(!config.lock_path().exists());

        // an unreadable lock aborts
        std::fs::write(config.lock_path(), "not a timestamp").unwrap();
        let err = lock(&config).unwrap_err();
        assert_eq!(err.0, "Cannot read logfile age - aborting.");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn session_saves_even_when_dropped_early() {
        let dir = temp_dir();
        let store = StateStore::new(test_config(&dir));
        {
            let mut session = store.session();
            session.state().seen_urls.add("https://x.example/1", None);
            // dropped without any explicit save
        }
        let loaded = store.load();
        assert!(loaded.seen_urls.contains("https://x.example/1"));
        std::fs::remove_dir_all(&dir).ok();
    }
}
