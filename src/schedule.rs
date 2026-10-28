//! A cron-like schedule: five fields — minute, hour, day of month, month,
//! day of week — with `*`, lists, ranges, `*/step`, names, and the
//! `@hourly`, `@daily`, and `@weekly` presets. The parsing and the search
//! are croner's; this module is the adapter that keeps the API the rest
//! of the crate sees: an expression in, the next firing instant out.
//!
//! Times are evaluated in the local timezone (the caller hands
//! [`Schedule::next_after`] times from `timeutil::now_local`, and croner
//! searches in `chrono::Local`, whose timezone database resolves
//! daylight-saving transitions): a run scheduled in the spring-forward
//! gap fires just after the gap, and a `@daily` midnight stays at
//! midnight, wall time, on both change days.

use crate::config::Config;
use croner::Cron;
use croner::parser::{CronParser, Seconds};

/// How often the schedulers check whether a firing minute has passed.
/// Run granularity is whole minutes, so a poll this fast is free, and a
/// firing — or the end of an external run blocking it — is picked up
/// within a second of the minute it belongs to.
pub const POLL: std::time::Duration = std::time::Duration::from_secs(1);

/// A parsed schedule: the croner pattern, plus the expression as it was
/// given, for the log lines.
#[derive(Clone, Debug)]
pub struct Schedule {
    cron: Cron,
    expression: String,
}

impl Schedule {
    /// Parse a schedule: up to five fields (missing trailing fields mean
    /// "every", so `*/15` alone is every 15 minutes), or one of the `@`
    /// presets. An empty text is not a schedule — the config treats it
    /// as "off".
    pub fn parse(text: &str) -> Result<Schedule, String> {
        let text = text.trim();
        let fields = match text {
            "@hourly" => "0 * * * *",
            "@daily" | "@midnight" => "0 0 * * *",
            "@weekly" => "0 0 * * 0",
            _ => text,
        };
        let parts: Vec<&str> = fields.split_whitespace().collect();
        if parts.is_empty() {
            return Err(format!("empty schedule: {text:?}"));
        }
        if parts.len() > 5 {
            return Err(format!(
                "expected at most five fields (minute hour day-of-month month day-of-week), found {}: {text}",
                parts.len()
            ));
        }
        // the padded form is what croner sees; the expression is kept as
        // it was given, for the log lines
        let mut padded: Vec<&str> = parts.clone();
        padded.resize(5, "*");
        let padded = padded.join(" ");
        let cron = CronParser::builder()
            .seconds(Seconds::Optional)
            .build()
            .parse(&padded)
            .map_err(|ex| format!("{text:?} is not a schedule: {ex}"))?;
        Ok(Schedule {
            cron,
            expression: text.to_string(),
        })
    }

    /// The expression as it was given, for the log lines.
    pub fn expression(&self) -> &str {
        &self.expression
    }

    /// The next whole minute, strictly after `after`, when the schedule
    /// fires. Returns nothing when nothing matches within croner's
    /// search limit.
    pub fn next_after(&self, after: time::OffsetDateTime) -> Option<time::OffsetDateTime> {
        let after: chrono::DateTime<chrono::Local> =
            chrono::DateTime::from_timestamp(after.unix_timestamp(), 0)?
                .with_timezone(&chrono::Local);
        let next = self.cron.find_next_occurrence(&after, false).ok()?;
        time::OffsetDateTime::from_unix_timestamp(next.timestamp()).ok()
    }

    /// Whether a firing minute fell in `(last_poll, now]` — the window a
    /// poller just covered. The schedulers poll every [`POLL`], re-reading
    /// the config each time, so an edit to the schedule is picked up on
    /// the next poll, and a firing that passed while a run was going is
    /// skipped rather than replayed.
    pub fn due_in_window(
        &self,
        last_poll: time::OffsetDateTime,
        now: time::OffsetDateTime,
    ) -> bool {
        matches!(self.next_after(last_poll), Some(firing) if firing <= now)
    }
}

/// The headless scheduler: the same polling serve mode's background tick
/// does, with no web UI. Every [`POLL`] the config's schedule is checked,
/// and a due run runs — one at a time, each recording itself in the run
/// history — until the process is stopped.
pub async fn forever(config: Config, db: sqlx::SqlitePool) {
    let mut last_poll = crate::timeutil::now_local();
    loop {
        tokio::time::sleep(POLL).await;
        let now = crate::timeutil::now_local();
        let due = config
            .run_schedule
            .as_deref()
            .and_then(|text| Schedule::parse(text).ok())
            .map(|schedule| schedule.due_in_window(last_poll, now))
            .unwrap_or(false);
        if due {
            // an external cron job, or another FediFetcher process, may
            // be mid-run: firing into its lock would record a failed
            // run, so the firing skips instead — the next firing comes
            // when the schedule says so
            if crate::store::a_process_holds_lock(&config) {
                tracing::info!(
                    "Scheduled run: another FediFetcher process holds the lock; trigger skipped"
                );
            } else {
                tracing::info!(
                    "Scheduled run: {}",
                    config.run_schedule.as_deref().unwrap_or_default()
                );
                // the run records itself in the run history, start to end
                let cancel = crate::run::CancelToken::new();
                let pause = crate::run::Pause::new();
                if let Err(ex) = crate::run::run_once(config.clone(), cancel, pause, &db).await {
                    tracing::error!("{ex}");
                }
            }
        }
        last_poll = now;
    }
}

#[cfg(test)]
mod tests;
