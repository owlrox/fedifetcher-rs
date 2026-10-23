//! One run of FediFetcher: the lock, the state, and the tasks.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use sqlx::SqlitePool;
use time::OffsetDateTime;
use tracing::{debug, error, info, warn};
use uuid::Uuid;

use crate::api::mastodon::HomeServer;
use crate::config::Config;
use crate::error::Result;
use crate::http::{build_callback_url, HttpClient};
use crate::store::{lock, Session, StateStore, ROBOTS_MAX_AGE};
use crate::tasks::{run_enabled_tasks, Context};
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
    cancelled: Arc<AtomicBool>,
}

impl CancelToken {
    pub fn new() -> Self {
        CancelToken::default()
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }

    /// Clear the flag, so the next run starts un-stopped.
    pub fn reset(&self) {
        self.cancelled.store(false, Ordering::Relaxed);
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Relaxed)
    }
}

/// Do one run, and report the message it finished with.
///
/// The state is written back even when a task fails part way through, and the
/// lock is released either way. A run whose `cancel` was set finishes early
/// and reports "Stopped.". Posts found private are recorded in `db`, so
/// later runs skip them without asking again.
pub async fn run_once(config: Config, cancel: CancelToken, db: &SqlitePool) -> Result<String> {
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
            return Err(ex.into());
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

    let result = run_tasks(&config, &http, &mut session, &cancel, db).await;

    // the session writes the state back, failures included
    if let Err(ex) = session.finish().await {
        error!("Could not write the state to the database: {ex}");
    }
    drop(lock);

    if cancel.is_cancelled() {
        let message = "Stopped.".to_string();
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
    db: &SqlitePool,
) -> Result<()> {
    if cancel.is_cancelled() {
        return Ok(());
    }
    let tasks = config.enabled_tasks();
    if tasks.is_empty() {
        info!("No tasks are enabled; nothing to do");
    } else {
        debug!("Enabled tasks: {}", tasks.join(", "));
    }
    for token in &config.access_tokens {
        if cancel.is_cancelled() {
            break;
        }
        let home = HomeServer::new(&config.server, token, http.clone());
        let mut ctx = Context {
            config,
            state: session.state(),
            http,
            home: &home,
            cancel: cancel.clone(),
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
        assert!(!token.is_cancelled());

        let clone = token.clone();
        token.cancel();
        assert!(token.is_cancelled());
        assert!(clone.is_cancelled(), "clones share the flag");

        token.reset();
        assert!(!clone.is_cancelled(), "reset clears it for all clones");
    }
}
