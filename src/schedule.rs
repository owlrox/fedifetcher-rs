//! A cron-like schedule: five fields — minute, hour, day of month, month,
//! day of week — with `*`, lists, ranges, and `*/step`, plus the
//! `@hourly`, `@daily`, and `@weekly` presets. Hand-rolled, so the single
//! binary grows no dependency: the surface is small and the tests are
//! exact. (A cron crate — croner — was evaluated and declined: its
//! semantics matched ours on every probe, but its daylight-saving
//! handling returns an ambiguous time too, and it would bring a second
//! time crate along for it.) Schedules are evaluated in the local
//! timezone by the caller,
//! who hands `next_after` times from `timeutil::now_local` — which means
//! the timezone is read once per poll, not tracked across it, so a run
//! scheduled near a daylight-saving change fires an hour off wall time
//! on the change day itself, and never otherwise.

use std::collections::BTreeSet;

/// How often the schedulers check whether a firing minute has passed.
/// Run granularity is whole minutes, so this is far finer than the
/// schedules themselves.
pub const POLL: time::Duration = time::Duration::seconds(1);

use time::{Date, Duration, Month, OffsetDateTime, Time};

/// A parsed schedule: every field expanded to the whole minutes it can
/// fire on. The expression is kept for the log lines.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Schedule {
    minutes: BTreeSet<u8>,
    hours: BTreeSet<u8>,
    days: Days,
    months: BTreeSet<u8>,
    expression: String,
}

/// The day-of-month/day-of-week pair, matched with OR when both are
/// restricted — vixie cron's rule, so `0 0 13 * 5` fires on the 13th and
/// on every Friday.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Days {
    Every,
    OfMonth(BTreeSet<u8>),
    OfWeek(BTreeSet<u8>),
    AnyOf {
        of_month: BTreeSet<u8>,
        of_week: BTreeSet<u8>,
    },
}

impl Schedule {
    /// Parse a schedule: five fields, or one of the `@` presets. An empty
    /// text is not a schedule — the config treats it as "off".
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
        // a short expression pads the missing trailing fields with `*`:
        // `*/15` alone means every 15 minutes, `30 4` means daily at 04:30
        let mut padded: Vec<&str> = parts.clone();
        padded.resize(5, "*");
        let minutes = parse_field(padded[0], 0, 59)?;
        let hours = parse_field(padded[1], 0, 23)?;
        let of_month = parse_field(padded[2], 1, 31)?;
        let months = parse_field(padded[3], 1, 12)?;
        let of_week = parse_field(padded[4], 0, 7)?;
        // 7 is Sunday too
        let of_week: BTreeSet<u8> = of_week
            .into_iter()
            .map(|day| if day == 7 { 0 } else { day })
            .collect();
        let days = match (padded[2] == "*", padded[4] == "*") {
            (true, true) => Days::Every,
            (false, true) => Days::OfMonth(of_month),
            (true, false) => Days::OfWeek(of_week),
            (false, false) => Days::AnyOf { of_month, of_week },
        };
        Ok(Schedule {
            minutes,
            hours,
            days,
            months,
            expression: text.to_string(),
        })
    }

    /// Whether a firing minute fell in `(last_poll, now]` — the window a
    /// poller just covered. The schedulers poll every [`POLL`], re-reading
    /// the config each time, so an edit to the schedule is picked up on
    /// the next poll, and a firing that passed while a run was going is
    /// skipped rather than replayed.
    pub fn due_in_window(&self, last_poll: OffsetDateTime, now: OffsetDateTime) -> bool {
        matches!(self.next_after(last_poll), Some(firing) if firing <= now)
    }

    /// The expression as it was given, for the log lines.
    pub fn expression(&self) -> &str {
        &self.expression
    }

    /// The next whole minute, strictly after `after`, when the schedule
    /// fires. Returns nothing when nothing matches within five years.
    pub fn next_after(&self, after: OffsetDateTime) -> Option<OffsetDateTime> {
        // the next whole minute: one minute on, seconds dropped
        let mut t = after.checked_add(Duration::minutes(1))?;
        t = t.replace_time(Time::from_hms(t.hour(), t.minute(), 0).ok()?);
        let limit = after + Duration::days(366 * 5);
        while t <= limit {
            if !self.months.contains(&(u8::from(t.month()))) {
                t = first_of_next_month(t)?;
                continue;
            }
            if !self.day_matches(t) {
                t = t
                    .replace_time(Time::MIDNIGHT)
                    .checked_add(Duration::days(1))?;
                continue;
            }
            if !self.hours.contains(&t.hour()) {
                t = next_hour(t)?;
                continue;
            }
            if !self.minutes.contains(&t.minute()) {
                t = t.checked_add(Duration::minutes(1))?;
                continue;
            }
            return Some(t);
        }
        None
    }

    /// Whether the day fields match: OR'd day-of-month and day-of-week
    /// when both are restricted, as vixie cron does.
    fn day_matches(&self, t: OffsetDateTime) -> bool {
        let of_month = t.day();
        // time's weekday counts Monday first; cron counts Sunday first
        let of_week = t.weekday().number_days_from_sunday();
        match &self.days {
            Days::Every => true,
            Days::OfMonth(days) => days.contains(&of_month),
            Days::OfWeek(days) => days.contains(&of_week),
            Days::AnyOf {
                of_month: days,
                of_week: weekdays,
            } => days.contains(&of_month) || weekdays.contains(&of_week),
        }
    }
}

/// The first minute of the next month, in the same offset.
fn first_of_next_month(t: OffsetDateTime) -> Option<OffsetDateTime> {
    let (year, month) = if t.month() == Month::December {
        (t.year() + 1, Month::January)
    } else {
        (t.year(), Month::try_from(t.month() as u8 + 1).ok()?)
    };
    Some(
        Date::from_calendar_date(year, month, 1)
            .ok()?
            .with_hms(0, 0, 0)
            .ok()?
            .assume_offset(t.offset()),
    )
}

/// The next hour on the hour — past midnight when the hour was 23.
fn next_hour(t: OffsetDateTime) -> Option<OffsetDateTime> {
    if t.hour() == 23 {
        t.replace_time(Time::MIDNIGHT)
            .checked_add(Duration::days(1))
    } else {
        Some(t.replace_time(Time::from_hms(t.hour() + 1, 0, 0).ok()?))
    }
}

/// The headless scheduler: the same polling serve mode's background tick
/// does, with no web UI. Every [`POLL`] the config's schedule is checked,
/// and a due run runs — one at a time, each recording itself in the run
/// history — until the process is stopped.
pub async fn forever(config: crate::config::Config, db: sqlx::SqlitePool) {
    let mut last_poll = crate::timeutil::now_local();
    loop {
        tokio::time::sleep(POLL.try_into().expect("a positive poll interval")).await;
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

/// One field: a comma-separated list of values, ranges, and steps.
fn parse_field(field: &str, min: u8, max: u8) -> Result<BTreeSet<u8>, String> {
    let mut values = BTreeSet::new();
    for part in field.split(',') {
        let (range, step) = match part.split_once('/') {
            Some((range, step)) => {
                let step: u32 = step
                    .parse()
                    .map_err(|_| format!("{part:?} does not have a step count"))?;
                if step == 0 {
                    return Err(format!("{part:?} has a step of zero"));
                }
                (range, step)
            }
            None => (part, 1),
        };
        let (lo, hi) = if range == "*" {
            (u32::from(min), u32::from(max))
        } else if let Some((lo, hi)) = range.split_once('-') {
            (parse_value(lo, part)?, parse_value(hi, part)?)
        } else {
            let value = parse_value(range, part)?;
            // `5/10` means 5 to the end, every 10, as vixie cron reads it
            (value, if step > 1 { u32::from(max) } else { value })
        };
        if lo < u32::from(min) || hi > u32::from(max) || lo > hi {
            return Err(format!("{part:?} is out of range {min}-{max}"));
        }
        let mut value = lo;
        while value <= hi {
            values.insert(value as u8);
            value += step;
        }
    }
    Ok(values)
}

fn parse_value(text: &str, part: &str) -> Result<u32, String> {
    text.parse()
        .map_err(|_| format!("{part:?} is not a number or range"))
}

#[cfg(test)]
mod tests;
