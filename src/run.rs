//! One run of FediFetcher: the lock, the state, and the tasks.

use time::OffsetDateTime;
use tracing::{error, info};
use uuid::Uuid;

use crate::api::mastodon::HomeServer;
use crate::config::Config;
use crate::error::Result;
use crate::http::{HttpClient, build_callback_url};
use crate::store::{ROBOTS_MAX_AGE, Session, StateStore, lock};
use crate::tasks::{Context, run_enabled_tasks};
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
        if let Err(ex) = self.http.get(&build_callback_url(url, &all), None, 5, 0.5).await {
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

/// Do one run, and report the message it finished with.
///
/// The state is written back even when a task fails part way through, and the
/// lock is released either way.
pub async fn run_once(config: Config) -> Result<String> {
    info!("Starting FediFetcher v{VERSION}");

    let http = HttpClient::new(config.clone());
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
    let mut session = store.session();

    // Delete any old robots.txt files so we can re-download them
    http.robots().discard_stale_files(ROBOTS_MAX_AGE);

    let result = run_tasks(&config, &http, &mut session).await;

    // the session saves the state on drop, failures included
    drop(session);
    drop(lock);

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

async fn run_tasks(config: &Config, http: &HttpClient, session: &mut Session) -> Result<()> {
    for token in &config.access_tokens {
        let home = HomeServer::new(&config.server, token, http.clone());
        let mut ctx = Context {
            config,
            state: session.state(),
            http,
            home: &home,
        };
        run_enabled_tasks(&mut ctx).await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notifier_reports_elapsed_milliseconds() {
        let config = Config::default();
        let http = HttpClient::new(config.clone());
        let notifier = Notifier::new(config, http, Uuid::new_v4());
        assert!(notifier.elapsed_ms() >= 0);
    }
}
