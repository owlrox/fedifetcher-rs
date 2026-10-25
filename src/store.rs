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
use crate::error::{Error, Result};
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
pub struct RepliedToot {
    pub url: String,
    pub post_ref: PostRef,
}

/// Everything FediFetcher remembers between runs, plus this run's scratch
/// space. The collections are private: reads and updates go through the
/// accessor pairs below, so every way they change is visible in one place.
#[derive(Default)]
pub struct State {
    seen_urls: TimestampedSet,
    known_followings: TimestampedSet,
    recently_checked_users: TimestampedSet,
    seen_hosts: ServerCache,
    recently_checked_context: ContextCache,
    replied_toot_server_ids: IndexMap<String, Option<RepliedToot>>,

    /// Rebuilt on load, and not written back out.
    all_known_users: TimestampedSet,
    parsed_urls: IndexMap<String, Option<PostRef>>,
}

impl State {
    pub fn seen_urls(&self) -> &TimestampedSet {
        &self.seen_urls
    }

    pub fn seen_urls_mut(&mut self) -> &mut TimestampedSet {
        &mut self.seen_urls
    }

    pub fn known_followings(&self) -> &TimestampedSet {
        &self.known_followings
    }

    pub fn known_followings_mut(&mut self) -> &mut TimestampedSet {
        &mut self.known_followings
    }

    pub fn recently_checked_users(&self) -> &TimestampedSet {
        &self.recently_checked_users
    }

    pub fn recently_checked_users_mut(&mut self) -> &mut TimestampedSet {
        &mut self.recently_checked_users
    }

    pub fn seen_hosts(&self) -> &ServerCache {
        &self.seen_hosts
    }

    pub fn seen_hosts_mut(&mut self) -> &mut ServerCache {
        &mut self.seen_hosts
    }

    pub fn recently_checked_context(&self) -> &ContextCache {
        &self.recently_checked_context
    }

    pub fn recently_checked_context_mut(&mut self) -> &mut ContextCache {
        &mut self.recently_checked_context
    }

    pub fn replied_toot_server_ids(&self) -> &IndexMap<String, Option<RepliedToot>> {
        &self.replied_toot_server_ids
    }

    pub fn replied_toot_server_ids_mut(&mut self) -> &mut IndexMap<String, Option<RepliedToot>> {
        &mut self.replied_toot_server_ids
    }

    /// Rebuilt on load, and not written back out.
    pub fn all_known_users(&self) -> &TimestampedSet {
        &self.all_known_users
    }

    pub fn all_known_users_mut(&mut self) -> &mut TimestampedSet {
        &mut self.all_known_users
    }

    pub fn parsed_urls(&self) -> &IndexMap<String, Option<PostRef>> {
        &self.parsed_urls
    }

    pub fn parsed_urls_mut(&mut self) -> &mut IndexMap<String, Option<PostRef>> {
        &mut self.parsed_urls
    }
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
            .recently_checked_users_mut()
            .expire_older_than(Duration::hours(config.remember_users_for_hours));
        state.seen_hosts_mut().expire(
            Duration::days(config.remember_hosts_for_days),
            SERVER_FAILURE_MAX_AGE,
        );
        state
            .recently_checked_context_mut()
            .expire_older_than(CONTEXT_MAX_AGE);

        for user in state.known_followings().iter().cloned().collect::<Vec<_>>() {
            state.all_known_users_mut().add(&user, None);
        }
        for user in state
            .recently_checked_users()
            .iter()
            .cloned()
            .collect::<Vec<_>>()
        {
            state.all_known_users_mut().add(&user, None);
        }
        Ok(state)
    }

    /// Write the state back, keeping only the newest `MAX_ENTRIES` of the
    /// append-only collections.
    pub async fn save(&self, pool: &SqlitePool, state: &State) -> Result<()> {
        db::save_state(pool, state, MAX_ENTRIES).await
    }

    /// Apply the retention rules a load applies — users, host info and
    /// context entries past their windows, robots.txt entries past their
    /// refetch age, and log lines beyond the retention — right now, so the
    /// database does not have to wait for the next run to shrink. The
    /// configuration, the OAuth rows, the run history, and the accounts'
    /// recorded wishes (opt-outs and private posts) are never touched.
    pub async fn cleanup(&self, pool: &SqlitePool, keep_logs: i64) -> Result<CleanupReport> {
        let config = &self.config;
        let mut state = db::load_state(pool).await?;

        let users = state
            .recently_checked_users_mut()
            .expire_older_than(Duration::hours(config.remember_users_for_hours));
        let hosts = state.seen_hosts_mut().expire(
            Duration::days(config.remember_hosts_for_days),
            SERVER_FAILURE_MAX_AGE,
        );
        let context = state
            .recently_checked_context_mut()
            .expire_older_than(CONTEXT_MAX_AGE);
        db::save_state(pool, &state, MAX_ENTRIES).await?;

        let logs = db::prune_logs(pool, keep_logs).await?;
        let robots = db::robots_discard_stale(pool, ROBOTS_MAX_AGE).await?;
        Ok(CleanupReport {
            logs,
            robots,
            hosts: hosts as u64,
            context: context as u64,
            users: users as u64,
        })
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

/// What cleaning the database removed, so the debug page can report it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CleanupReport {
    /// Log lines beyond the retention.
    pub logs: u64,
    /// robots.txt entries past their refetch age.
    pub robots: u64,
    /// Host entries past the configured remember window.
    pub hosts: u64,
    /// Context entries past their week.
    pub context: u64,
    /// Recently checked users past the configured remember window.
    pub users: u64,
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
        let pid = value
            .get("pid")
            .and_then(Value::as_u64)
            .map(|pid| pid as u32);
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
pub fn lock(config: &Config) -> Result<LockGuard> {
    let path = config.lock_path();

    if path.exists() {
        debug!("Lock file exists at {}", path.display());
        let content = std::fs::read_to_string(&path)
            .ok()
            .and_then(|text| parse_lock_content(&text))
            .ok_or_else(|| {
                Error::Locked(format!(
                    "Cannot read lock file {} - aborting.",
                    path.display()
                ))
            })?;

        let age = timeutil::now() - content.created;
        if age < Duration::hours(config.lock_hours) {
            match content.pid {
                Some(pid) if process_alive(pid) => {
                    return Err(Error::Locked(format!(
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
    std::fs::write(&path, content.to_string()).map_err(|ex| Error::Io {
        context: format!("Cannot write lock file {}", path.display()),
        source: ex,
    })?;
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
mod tests;
