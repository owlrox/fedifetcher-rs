//! What FediFetcher remembers between runs, kept in the database, and the
//! lock file that keeps two runs from overlapping.
//!
//! The state used to live in text files in the state directory; it now lives
//! in the database's state tables, one table per collection. The lock stays
//! a file: a lock has to work between processes, and a SQLite write lock
//! would block the whole database for as long as a run holds it.

use std::path::PathBuf;

use indexmap::IndexMap;
use serde_json::Value;
use sqlx::SqlitePool;
use time::{Duration, OffsetDateTime};
use tracing::{debug, warn};

use crate::config::Config;
use crate::db;
use crate::error::{LockedError, Result};
use crate::state::{ContextCache, ServerCache, TimestampedSet};
use crate::timeutil;
use crate::urls::PostRef;

/// How many entries of the append-only collections to carry over between runs.
pub const MAX_ENTRIES: usize = 100_000;

pub const CONTEXT_MAX_AGE: Duration = Duration::days(7);
pub const ROBOTS_MAX_AGE: Duration = Duration::days(1);
pub const SERVER_FAILURE_MAX_AGE: Duration = Duration::hours(1);

/// A post that was replied to: the URL we followed, and where it landed.
///
/// Stored in the `replied_toots` table; a lookup that found nothing is
/// remembered as a row of NULLs, so it is not asked about again.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RepliedToot(pub String, pub PostRef);

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

/// Reads and writes what FediFetcher remembers, in the database.
pub struct StateStore {
    config: Config,
}

impl StateStore {
    pub fn new(config: Config) -> Self {
        StateStore { config }
    }

    /// Load the state, applying the configured expiry: users remembered
    /// longer ago than `remember_users_for_hours` are forgotten, host info
    /// older than `remember_hosts_for_days`, and context posts not checked
    /// in a week. `all_known_users` is rebuilt from the followings and the
    /// recently checked users.
    pub async fn load(&self, pool: &SqlitePool) -> Result<State> {
        let config = &self.config;
        let mut state = db::load_state(pool).await?;

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
        Ok(state)
    }

    /// Write the state back, keeping only the newest `MAX_ENTRIES` of the
    /// append-only collections.
    pub async fn save(&self, pool: &SqlitePool, state: &State) -> Result<()> {
        db::save_state(pool, state, MAX_ENTRIES).await
    }

    /// Load state into a session, to be written back by `Session::finish`.
    pub async fn session(&self, pool: &SqlitePool) -> Result<Session> {
        Ok(Session {
            store: StateStore {
                config: self.config.clone(),
            },
            pool: pool.clone(),
            state: Some(self.load(pool).await?),
        })
    }
}

/// A loaded state, written back by `finish` when the run ends. Dropping a
/// session without finishing it loses the run's changes; `finish` is called
/// on every path out of a run, success or failure.
pub struct Session {
    store: StateStore,
    pool: SqlitePool,
    state: Option<State>,
}

impl Session {
    pub fn state(&mut self) -> &mut State {
        self.state.as_mut().expect("session state already taken")
    }

    /// Write the state back to the database.
    pub async fn finish(mut self) -> Result<()> {
        if let Some(state) = self.state.take() {
            self.store.save(&self.pool, &state).await?;
        }
        Ok(())
    }
}

/// What the lock file records: who holds it, and since when.
struct LockContent {
    pid: Option<u32>,
    created: OffsetDateTime,
}

/// Read a lock file's content: the JSON this version writes, or the bare
/// timestamp older versions wrote (whose holder cannot be identified).
fn parse_lock_content(text: &str) -> Option<LockContent> {
    if let Ok(value) = serde_json::from_str::<Value>(text) {
        let created = value
            .get("created")
            .and_then(Value::as_str)
            .and_then(timeutil::parse)?;
        let pid = value.get("pid").and_then(Value::as_u64).map(|pid| pid as u32);
        Some(LockContent { pid, created })
    } else {
        timeutil::parse(text.trim()).map(|created| LockContent { pid: None, created })
    }
}

/// Whether the process is still running. Unknown on non-Unix, where the
/// caller falls back to the lock's age alone.
#[cfg(unix)]
fn process_alive(pid: u32) -> bool {
    // signal 0 checks existence without sending anything; EPERM still means
    // the process exists, so only a zero return counts as alive here
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
}

#[cfg(not(unix))]
fn process_alive(_pid: u32) -> bool {
    true
}

/// Hold the lock file for the duration, so two runs cannot overlap.
///
/// The lock records the holder's process id, and only blocks while that
/// process is alive. A lock whose holder is dead — a run that was killed, or
/// crashed — is reclaimed on the spot, as is one older than `lock_hours`, and
/// one too old to name a holder (written by a previous version), so a single
/// long-running service never has to wait out a lock it left behind.
pub fn lock(config: &Config) -> std::result::Result<LockGuard, LockedError> {
    let path = config.lock_path();

    if path.exists() {
        debug!("Lock file exists at {}", path.display());
        let content = std::fs::read_to_string(&path)
            .ok()
            .and_then(|text| parse_lock_content(&text))
            .ok_or_else(|| {
                LockedError(format!(
                    "Cannot read lock file {} - aborting.",
                    path.display()
                ))
            })?;

        let age = timeutil::now() - content.created;
        if age < Duration::hours(config.lock_hours) {
            match content.pid {
                Some(pid) if process_alive(pid) => {
                    return Err(LockedError(format!(
                        "Lock file age is {} - below --lock-hours={} provided: held by process {} since {} ({}). Another FediFetcher is running.",
                        timeutil::fmt_timedelta(age),
                        config.lock_hours,
                        pid,
                        timeutil::fmt_lock_ts(content.created),
                        path.display(),
                    )));
                }
                Some(pid) => {
                    warn!(
                        "Lock file {} is held by dead process {} (lock age {}); reclaiming it",
                        path.display(),
                        pid,
                        timeutil::fmt_timedelta(age),
                    );
                }
                None => {
                    warn!(
                        "Lock file {} was left by a version that did not record its process (lock age {}); reclaiming it",
                        path.display(),
                        timeutil::fmt_timedelta(age),
                    );
                }
            }
        } else {
            debug!("Lock file has expired. Removed lock file.");
        }

        std::fs::remove_file(&path).ok();
    }

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let content = serde_json::json!({
        "pid": std::process::id(),
        "created": timeutil::fmt_lock_ts(timeutil::now()),
    });
    std::fs::write(&path, content.to_string())
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

    #[tokio::test]
    async fn state_round_trips_through_the_database() {
        let dir = temp_dir();
        let config = test_config(&dir);
        let pool = db::open(&config).await.unwrap();
        let store = StateStore::new(config);

        let mut session = store.session(&pool).await.unwrap();
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
        session.finish().await.unwrap();

        let loaded = store.load(&pool).await.unwrap();
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

    #[tokio::test]
    async fn dropping_a_session_without_finishing_loses_the_changes() {
        let dir = temp_dir();
        let config = test_config(&dir);
        let pool = db::open(&config).await.unwrap();
        let store = StateStore::new(config);
        {
            let mut session = store.session(&pool).await.unwrap();
            session.state().seen_urls.add("https://x.example/1", None);
            // dropped without finish: the run's changes are lost
        }
        let loaded = store.load(&pool).await.unwrap();
        assert!(loaded.seen_urls.is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn load_applies_the_configured_expiry() {
        let dir = temp_dir();
        let mut config = test_config(&dir);
        config.remember_users_for_hours = 24;
        let pool = db::open(&config).await.unwrap();
        let old = timeutil::fmt_state_ts(timeutil::now() - Duration::days(10));
        let fresh = timeutil::fmt_state_ts(timeutil::now());
        for (acct, first_seen) in [("old@remote.example", &old), ("fresh@remote.example", &fresh)] {
            sqlx::query("INSERT INTO recently_checked_users (acct, first_seen) VALUES (?, ?)")
                .bind(acct)
                .bind(first_seen)
                .execute(&pool)
                .await
                .unwrap();
        }

        let store = StateStore::new(config);
        let loaded = store.load(&pool).await.unwrap();
        assert!(
            !loaded.recently_checked_users.contains("old@remote.example"),
            "a user remembered longer ago than remember_users_for_hours is forgotten"
        );
        assert!(loaded.recently_checked_users.contains("fresh@remote.example"));
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
        assert!(
            err.0.starts_with("Cannot read lock file"),
            "{}",
            err.0
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// A lock in the old format — a bare timestamp, no recorded process —
    /// cannot name a holder, so it is reclaimed rather than blocking.
    #[test]
    fn a_legacy_lock_without_a_recorded_process_is_reclaimed() {
        let dir = temp_dir();
        let config = test_config(&dir);

        std::fs::write(
            config.lock_path(),
            timeutil::fmt_lock_ts(timeutil::now()),
        )
        .unwrap();
        let guard = lock(&config).expect("a legacy lock is reclaimed");
        assert!(config.lock_path().exists());
        drop(guard);
        assert!(!config.lock_path().exists());

        std::fs::remove_dir_all(&dir).ok();
    }

    /// A lock held by a process that no longer exists is reclaimed.
    #[cfg(unix)]
    #[test]
    fn a_lock_held_by_a_dead_process_is_reclaimed() {
        let dir = temp_dir();
        let config = test_config(&dir);

        // a process that is dead by the time the lock is read
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .unwrap();
        let dead_pid = child.id();
        child.kill().unwrap();
        child.wait().unwrap();
        assert!(!process_alive(dead_pid));

        std::fs::write(
            config.lock_path(),
            serde_json::json!({"pid": dead_pid, "created": timeutil::fmt_lock_ts(timeutil::now())})
                .to_string(),
        )
        .unwrap();
        let guard = lock(&config).expect("a dead holder's lock is reclaimed");
        assert!(config.lock_path().exists());
        drop(guard);

        std::fs::remove_dir_all(&dir).ok();
    }

    /// A lock held by a live process blocks, and says who holds it.
    #[cfg(unix)]
    #[test]
    fn a_lock_held_by_a_live_process_blocks_and_names_it() {
        let dir = temp_dir();
        let config = test_config(&dir);

        let live_pid = std::process::id();
        std::fs::write(
            config.lock_path(),
            serde_json::json!({"pid": live_pid, "created": timeutil::fmt_lock_ts(timeutil::now())})
                .to_string(),
        )
        .unwrap();
        let err = lock(&config).unwrap_err();
        assert!(err.0.contains("below --lock-hours=24"), "{}", err.0);
        assert!(
            err.0.contains(&format!("held by process {live_pid}")),
            "{}",
            err.0
        );
        assert!(
            config.lock_path().exists(),
            "the live holder's lock is not removed"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn lock_content_reads_both_formats() {
        let legacy = parse_lock_content(&timeutil::fmt_lock_ts(timeutil::now())).unwrap();
        assert_eq!(legacy.pid, None);

        let current = parse_lock_content(
            &serde_json::json!({"pid": 123, "created": timeutil::fmt_lock_ts(timeutil::now())})
                .to_string(),
        )
        .unwrap();
        assert_eq!(current.pid, Some(123));

        assert!(parse_lock_content("not a timestamp").is_none());
    }

}
