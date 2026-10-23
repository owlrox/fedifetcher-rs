//! One run of FediFetcher: the lock, the state, and the tasks.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use sqlx::SqlitePool;
use time::OffsetDateTime;
use tracing::{debug, error, info, warn};
use uuid::Uuid;

use crate::api::mastodon::HomeServer;
use crate::config::Config;
use crate::db;
use crate::error::Result;
use crate::http::{build_callback_url, HttpClient};
use crate::store::{lock, Session, State, StateStore, ROBOTS_MAX_AGE};
use crate::tasks::run_enabled_tasks;
use crate::timeutil;
use crate::VERSION;

/// Pings the configured callback urls, for dead man's switch monitoring.
pub struct Notifier {
    config: Config,
    http: HttpClient,
    run_id: Uuid,
    started: OffsetDateTime,
}

impl Notifier {
    pub fn new(config: Config, http: HttpClient, run_id: Uuid) -> Self {
        Notifier {
            http: http.ignoring_robots(),
            config,
            run_id,
            started: timeutil::now(),
        }
    }

    fn elapsed(&self) -> time::Duration {
        timeutil::now() - self.started
    }

    fn elapsed_ms(&self) -> i64 {
        self.elapsed().whole_milliseconds() as i64
    }

    async fn ping(&self, url: Option<&str>, params: Vec<(String, String)>) {
        let Some(url) = url else {
            return;
        };
        let mut all = vec![("rid".to_string(), self.run_id.to_string())];
        all.extend(params);
        if let Err(ex) = self.http.get(&build_callback_url(url, &all), None).await {
            error!("Error getting callback url: {ex}");
        }
    }

    pub async fn starting(&self) {
        self.ping(self.config.on_start.as_deref(), Vec::new()).await;
    }

    pub async fn done(&self, message: &str) {
        self.ping(
            self.config.on_done.as_deref(),
            vec![
                ("ping".to_string(), self.elapsed_ms().to_string()),
                ("msg".to_string(), message.to_string()),
            ],
        )
        .await;
    }

    pub async fn failed(&self, message: &str) {
        self.ping(
            self.config.on_fail.as_deref(),
            vec![
                ("ping".to_string(), self.elapsed_ms().to_string()),
                ("msg".to_string(), message.to_string()),
            ],
        )
        .await;
    }
}

/// A cooperative stop signal for a run: set from the web UI's stop button,
/// checked between tasks and between the items of the long loops.
///
/// Cloning shares the flag, so the web UI's copy and the run's copy see the
/// same stop. It cannot interrupt an in-flight HTTP request; a stop lands
/// within one request, bounded by the configured http timeout.
#[derive(Clone, Default)]
pub struct CancelToken {
    canceled: Arc<AtomicBool>,
}

impl CancelToken {
    pub fn new() -> Self {
        CancelToken::default()
    }

    pub fn cancel(&self) {
        self.canceled.store(true, Ordering::Relaxed);
    }

    /// Clear the flag, so the next run starts un-stopped.
    pub fn reset(&self) {
        self.canceled.store(false, Ordering::Relaxed);
    }

    pub fn is_canceled(&self) -> bool {
        self.canceled.load(Ordering::Relaxed)
    }
}

/// How often a paused run re-checks its signals: how quickly a resume or
/// a stop takes effect while the run is holding.
const PAUSE_POLL: std::time::Duration = std::time::Duration::from_millis(200);

/// A cooperative pause signal for a run: set from the web UI's pause
/// button, waited on between tasks and between the items of the long
/// loops.
///
/// Cloning shares the flag, so the web UI's copy and the run's copy see
/// the same pause. Like a stop, a pause cannot interrupt an in-flight
/// HTTP request — the run holds at its next checkpoint — and a stop lands
/// while paused.
#[derive(Clone, Default)]
pub struct Pause {
    paused: Arc<AtomicBool>,
}

impl Pause {
    pub fn new() -> Self {
        Pause::default()
    }

    pub fn pause(&self) {
        self.paused.store(true, Ordering::Relaxed);
    }

    /// Clear the flag, so a paused run continues — and the next run starts
    /// unpaused.
    pub fn resume(&self) {
        self.paused.store(false, Ordering::Relaxed);
    }

    pub fn is_paused(&self) -> bool {
        self.paused.load(Ordering::Relaxed)
    }

    /// Hold while paused, until the run is resumed or stopped.
    async fn wait(&self, cancel: &CancelToken) {
        while self.is_paused() && !cancel.is_canceled() {
            tokio::time::sleep(PAUSE_POLL).await;
        }
    }
}

/// Everything a run or a manual fetch needs: the settings, what we
/// remember, who to ask, and how to stop or hold it.
pub struct Context<'a> {
    pub config: &'a Config,
    pub state: &'a mut State,
    pub http: &'a HttpClient,
    pub home: &'a HomeServer,
    pub cancel: CancelToken,
    pub pause: Pause,
    pub db: &'a SqlitePool,
}

impl Context<'_> {
    /// The run's checkpoint, called between tasks and between the items of
    /// the long loops: hold while the run is paused, then report whether it
    /// was stopped. A stop during a pause still ends the run.
    pub async fn check_in(&self) -> bool {
        self.pause.wait(&self.cancel).await;
        self.cancel.is_canceled()
    }
}

/// Do one run, and report the message it finished with.
///
/// The run records itself in the run history, whichever process started it
/// — the web UI, or the command line — so the dashboard's status pill and
/// history see it while it is going.
///
/// The state is written back even when a task fails part way through, and the
/// lock is released either way. A run whose `cancel` was set finishes early
/// and reports how long it ran before stopping; a paused run holds at its
/// checkpoints until it is resumed or stopped. Posts found private are
/// recorded in `db`, so later runs skip them without asking again.
pub async fn run_once(
    config: Config,
    cancel: CancelToken,
    pause: Pause,
    db: &SqlitePool,
) -> Result<String> {
    let started = timeutil::now();
    let run_id = match db::start_run(db, &timeutil::fmt_state_ts(started)).await {
        Ok(id) => Some(id),
        Err(ex) => {
            warn!("Could not record the run's start in the database: {ex}");
            None
        }
    };
    let result = run_once_inner(config, cancel.clone(), pause, db).await;
    if let Some(id) = run_id {
        let (outcome, message) = match &result {
            Ok(message) if cancel.is_canceled() => ("stopped", message.clone()),
            Ok(message) => ("ok", message.clone()),
            Err(ex) => ("error", ex.to_string()),
        };
        if let Err(ex) = db::finish_run(
            db,
            id,
            &timeutil::fmt_state_ts(timeutil::now()),
            outcome,
            &message,
        )
        .await
        {
            warn!("Could not record the run's end in the database: {ex}");
        }
    }
    result
}

async fn run_once_inner(
    config: Config,
    cancel: CancelToken,
    pause: Pause,
    db: &SqlitePool,
) -> Result<String> {
    info!("Starting FediFetcher v{VERSION}");
    if !config.respect_robots_txt {
        warn!("respect_robots_txt is disabled: remote servers' robots.txt will not be respected");
    }
    if !config.respect_user_opt_out {
        warn!("respect_user_opt_out is disabled: accounts that have opted out will still be backfilled");
    }

    let http = HttpClient::new(config.clone(), db.clone());
    let notifier = Notifier::new(config.clone(), http.clone(), Uuid::new_v4());
    let store = StateStore::new(config.clone());

    notifier.starting().await;

    let lock = match lock(&config) {
        Ok(lock) => lock,
        Err(ex) => {
            // Python logs this as critical and exits without the usual failure log
            error!("{ex}");
            notifier.failed(&ex.to_string()).await;
            return Err(ex);
        }
    };
    let mut session = match store.session(db).await {
        Ok(session) => session,
        Err(ex) => {
            error!("{ex}");
            notifier.failed(&ex.to_string()).await;
            return Err(ex);
        }
    };

    // Delete any old robots.txt entries so we can re-download them
    http.robots().discard_stale(ROBOTS_MAX_AGE).await;

    let result = run_tasks(&config, &http, &mut session, &cancel, &pause, db).await;

    // the session writes the state back, failures included
    if let Err(ex) = session.finish().await {
        error!("Could not write the state to the database: {ex}");
    }
    drop(lock);

    if cancel.is_canceled() {
        let message = format!(
            "Stopped after {}.",
            timeutil::fmt_timedelta(notifier.elapsed())
        );
        notifier.done(&message).await;
        info!("{message}");
        return Ok(message);
    }

    match result {
        Ok(()) => {
            let message = format!(
                "Processing finished in {}.",
                timeutil::fmt_timedelta(notifier.elapsed())
            );
            notifier.done(&message).await;
            info!("{message}");
            Ok(message)
        }
        Err(ex) => {
            error!(
                "Job failed after {}.",
                timeutil::fmt_timedelta(notifier.elapsed())
            );
            notifier.failed(&ex.to_string()).await;
            Err(ex)
        }
    }
}

async fn run_tasks(
    config: &Config,
    http: &HttpClient,
    session: &mut Session,
    cancel: &CancelToken,
    pause: &Pause,
    db: &SqlitePool,
) -> Result<()> {
    if cancel.is_canceled() {
        return Ok(());
    }
    let tasks = config.enabled_tasks();
    if tasks.is_empty() {
        info!("No tasks are enabled; nothing to do");
    } else {
        debug!("Enabled tasks: {}", tasks.join(", "));
    }
    for token in &config.access_tokens {
        // hold between servers while paused; a stop during the pause ends it
        pause.wait(cancel).await;
        if cancel.is_canceled() {
            break;
        }
        let home = HomeServer::new(&config.server, token, http.clone());
        let mut ctx = Context {
            config,
            state: session.state(),
            http,
            home: &home,
            cancel: cancel.clone(),
            pause: pause.clone(),
            db,
        };
        run_enabled_tasks(&mut ctx).await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn notifier_reports_elapsed_milliseconds() {
        let config = Config::default();
        let http = HttpClient::new(config.clone(), crate::db::memory_pool());
        let notifier = Notifier::new(config, http, Uuid::new_v4());
        assert!(notifier.elapsed_ms() >= 0);
    }

    #[test]
    fn cancel_token_starts_unset_and_shares_its_flag() {
        let token = CancelToken::new();
        assert!(!token.is_canceled());

        let clone = token.clone();
        token.cancel();
        assert!(token.is_canceled());
        assert!(clone.is_canceled(), "clones share the flag");

        token.reset();
        assert!(!clone.is_canceled(), "reset clears it for all clones");
    }

    #[test]
    fn pause_starts_unpaused_and_shares_its_flag() {
        let pause = Pause::new();
        assert!(!pause.is_paused());

        let clone = pause.clone();
        pause.pause();
        assert!(pause.is_paused());
        assert!(clone.is_paused(), "clones share the flag");

        pause.resume();
        assert!(!clone.is_paused(), "resume clears it for all clones");
    }

    #[tokio::test]
    async fn check_in_holds_while_paused_until_resumed_or_stopped() {
        let config = Config::default();
        let db = crate::db::memory_pool();
        let http = HttpClient::new(config.clone(), db.clone());
        let home = HomeServer::new(&config.server, "token", http.clone());
        let mut state = State::default();
        let cancel = CancelToken::new();
        let pause = Pause::new();
        let ctx = Context {
            config: &config,
            state: &mut state,
            http: &http,
            home: &home,
            cancel: cancel.clone(),
            pause: pause.clone(),
            db: &db,
        };

        // neither paused nor stopped: passes straight through
        assert!(!ctx.check_in().await);

        // paused: holds, until resumed
        pause.pause();
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(100), ctx.check_in())
                .await
                .is_err(),
            "a paused run holds at the checkpoint"
        );
        pause.resume();
        assert!(
            !tokio::time::timeout(std::time::Duration::from_secs(1), ctx.check_in())
                .await
                .expect("resumed, so the checkpoint passes"),
            "a resumed run goes on"
        );

        // a stop lands even while paused
        pause.pause();
        cancel.cancel();
        assert!(
            ctx.check_in().await,
            "a stop during a pause still ends the run"
        );
    }
}
