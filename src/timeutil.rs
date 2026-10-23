//! Datetime handling.
//!
//! Timestamps are written in the `str(datetime)` shape
//! (`2026-10-22 01:23:45.123456+00:00`) for compatibility with state files
//! written by earlier versions. Both that shape and RFC 3339 are accepted
//! when reading.

use time::macros::format_description;
use time::{Duration, OffsetDateTime, PrimitiveDateTime, UtcOffset};

/// Current time, UTC.
pub fn now() -> OffsetDateTime {
    OffsetDateTime::now_utc()
}

/// Current time in the local timezone, falling back to UTC.
pub fn now_local() -> OffsetDateTime {
    OffsetDateTime::now_local().unwrap_or_else(|_| OffsetDateTime::now_utc())
}

/// The local UTC offset, falling back to UTC.
pub fn local_offset() -> UtcOffset {
    UtcOffset::current_local_offset().unwrap_or(UtcOffset::UTC)
}

fn local_naive_to_utc(dt: PrimitiveDateTime) -> OffsetDateTime {
    dt.assume_offset(local_offset()).to_offset(UtcOffset::UTC)
}

fn to_local(dt: OffsetDateTime) -> OffsetDateTime {
    dt.to_offset(local_offset())
}

const PY_TS_AWARE: &[time::format_description::FormatItem<'_>] = format_description!(
    "[year]-[month]-[day] [hour]:[minute]:[second][optional [.[subsecond digits:1+]]][optional [[offset_hour sign:mandatory]:[offset_minute]]]"
);
const PY_TS_NAIVE: &[time::format_description::FormatItem<'_>] = format_description!(
    "[year]-[month]-[day] [hour]:[minute]:[second][optional [.[subsecond digits:1+]]]"
);
const DATE_ONLY: &[time::format_description::FormatItem<'_>] =
    format_description!("[year]-[month]-[day]");

/// Read a timestamp in any of the shapes FediFetcher meets: RFC 3339 (what the
/// APIs send), the Python `str(datetime)` shape (what state files contain), a
/// bare date, or a unix timestamp (what `x-ratelimit-reset` often carries).
///
/// A timestamp without an offset is read as local time, which is how the
/// Python code treats them.
pub fn parse(value: &str) -> Option<OffsetDateTime> {
    let text = value.trim();
    if text.is_empty() {
        return None;
    }

    if text.bytes().all(|b| b.is_ascii_digit()) {
        if let Ok(secs) = text.parse::<i64>() {
            return OffsetDateTime::from_unix_timestamp(secs).ok();
        }
    }

    if let Ok(date) = PrimitiveDateTime::parse(text, DATE_ONLY) {
        return Some(local_naive_to_utc(date));
    }

    // The standard shape first — what the APIs send — needing no rewriting.
    if let Ok(dt) = OffsetDateTime::parse(text, &time::format_description::well_known::Rfc3339) {
        return Some(dt);
    }

    // Normalize the separators RFC 3339 allows, then try the Python shape.
    let mut normalized = text.replace('T', " ");
    if let Some(stripped) = normalized.strip_suffix('Z') {
        normalized = format!("{stripped}+00:00");
    }
    // An offset without a colon, e.g. +0000.
    if normalized.len() > 5 {
        let tail = &normalized[normalized.len() - 5..];
        if (tail.starts_with('+') || tail.starts_with('-'))
            && tail[1..].bytes().all(|b| b.is_ascii_digit())
        {
            normalized = format!(
                "{}{}:{}",
                &normalized[..normalized.len() - 5],
                &tail[..1],
                &tail[1..]
            );
        }
    }

    if let Ok(dt) = OffsetDateTime::parse(&normalized, PY_TS_AWARE) {
        return Some(dt);
    }
    if let Ok(dt) = PrimitiveDateTime::parse(&normalized, PY_TS_NAIVE) {
        return Some(local_naive_to_utc(dt));
    }
    None
}

/// Format a timestamp the way Python's `str(datetime)` does, in local time:
/// `2026-10-22 01:23:45.123456+00:00`, dropping the fraction when it is zero.
pub fn fmt_state_ts(dt: OffsetDateTime) -> String {
    let local = to_local(dt);
    format!(
        "{}{}",
        fmt_local_naive(local),
        offset_suffix(local.offset())
    )
}

/// Format a timestamp as a naive local datetime, the way the lock file is
/// written: `2026-10-22 01:23:45.123456`.
pub fn fmt_lock_ts(dt: OffsetDateTime) -> String {
    fmt_local_naive(to_local(dt))
}

/// A local timestamp without its offset: `2026-10-22 01:23:45.123456`.
fn fmt_local_naive(local: OffsetDateTime) -> String {
    let fraction = subsecond_fraction(local.microsecond());
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}{fraction}",
        local.year(),
        u8::from(local.month()),
        local.day(),
        local.hour(),
        local.minute(),
        local.second(),
    )
}

/// The `+00:00` suffix of a UTC offset.
fn offset_suffix(offset: UtcOffset) -> String {
    let (sign, hours, minutes) = offset_parts(offset);
    format!("{sign}{hours:02}:{minutes:02}")
}

/// The `.123456` fraction of a microsecond value, empty when it is zero —
/// the way Python's `str(datetime)` drops a zero fraction.
fn subsecond_fraction(micros: u32) -> String {
    if micros == 0 {
        String::new()
    } else {
        format!(".{micros:06}")
    }
}

fn offset_parts(offset: UtcOffset) -> (char, i8, i8) {
    let seconds = offset.whole_seconds();
    let sign = if seconds < 0 { '-' } else { '+' };
    let abs = seconds.abs();
    (sign, (abs / 3600) as i8, ((abs % 3600) / 60) as i8)
}

/// Format a duration the way Python's `str(timedelta)` does:
/// `1 day, 0:05:23.123456`, or `0:05:23` when there are no days.
pub fn fmt_timedelta(duration: Duration) -> String {
    let total_micros = duration.whole_microseconds().abs();
    let days = total_micros / 86_400_000_000;
    let rest = total_micros % 86_400_000_000;
    let hours = rest / 3_600_000_000;
    let rest = rest % 3_600_000_000;
    let minutes = rest / 60_000_000;
    let rest = rest % 60_000_000;
    let seconds = rest / 1_000_000;
    let micros = rest % 1_000_000;

    let fraction = subsecond_fraction(micros as u32);
    let hms = format!("{hours}:{minutes:02}:{seconds:02}{fraction}");
    if days == 0 {
        hms
    } else {
        let plural = if days == 1 { "day" } else { "days" };
        format!("{days} {plural}, {hms}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::datetime;

    #[test]
    fn parses_rfc3339() {
        let dt = parse("2026-10-22T01:23:45.123Z").unwrap();
        assert_eq!(dt, datetime!(2026-10-22 01:23:45.123 UTC));
    }

    #[test]
    fn parses_the_legacy_datetime_shape() {
        let dt = parse("2026-10-22 01:23:45.123456+00:00").unwrap();
        assert_eq!(dt, datetime!(2026-10-22 01:23:45.123456 UTC));
        let dt = parse("2026-10-22 01:23:45+00:00").unwrap();
        assert_eq!(dt, datetime!(2026-10-22 01:23:45 UTC));
    }

    #[test]
    fn parses_unix_timestamp() {
        let dt = parse("1730000000").unwrap();
        assert_eq!(dt.unix_timestamp(), 1_730_000_000);
    }

    #[test]
    fn state_ts_round_trips() {
        // The Python str(datetime) shape, with whatever the local offset is:
        // `2026-10-22 01:23:45.123456-05:00`, fraction dropped when zero.
        let dt = datetime!(2026-10-22 01:23:45.123456 UTC);
        let text = fmt_state_ts(dt);
        assert_eq!(text.len(), 32, "{text}");
        let bytes = text.as_bytes();
        assert_eq!(bytes[4], b'-');
        assert_eq!(bytes[7], b'-');
        assert_eq!(bytes[10], b' ');
        assert_eq!(bytes[13], b':');
        assert_eq!(bytes[16], b':');
        assert_eq!(bytes[19], b'.');
        assert!(bytes[26] == b'+' || bytes[26] == b'-', "{text}");
        assert_eq!(parse(&text).unwrap(), dt);

        let dt = datetime!(2026-10-22 01:23:45 UTC);
        let text = fmt_state_ts(dt);
        assert_eq!(text.len(), 25, "{text}");
        assert!(!text.contains('.'), "{text}");
        assert!(
            text.as_bytes()[19] == b'+' || text.as_bytes()[19] == b'-',
            "{text}"
        );
        assert_eq!(parse(&text).unwrap(), dt);
    }

    #[test]
    fn timedelta_formats_with_days_and_fraction() {
        assert_eq!(fmt_timedelta(Duration::seconds(5 * 60 + 23)), "0:05:23");
        assert_eq!(
            fmt_timedelta(Duration::days(1) + Duration::seconds(5)),
            "1 day, 0:00:05"
        );
        assert_eq!(
            fmt_timedelta(Duration::days(2) + Duration::microseconds(123_456)),
            "2 days, 0:00:00.123456"
        );
    }
}
