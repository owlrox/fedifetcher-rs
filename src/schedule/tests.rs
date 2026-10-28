//! The tests for `schedule.rs`. Kept in a child module so they see the
//! private items; the parent holds only the `mod tests;` pointer.

use super::*;
use time::macros::datetime;

fn parse(text: &str) -> Schedule {
    Schedule::parse(text).expect(text)
}

fn next(expression: &str, after: time::OffsetDateTime) -> time::OffsetDateTime {
    parse(expression)
        .next_after(after)
        .expect("the schedule fires within the search limit")
}

/// The instant of a local wall-clock time: what a schedule firing at
/// that wall time produces on this machine. The wall time is resolved
/// through chrono's timezone database, so a date on the other side of a
/// daylight-saving change gets the offset that date actually keeps.
fn local_wall(year: i32, month: u8, day: u8, hour: u8, minute: u8) -> time::OffsetDateTime {
    use chrono::TimeZone;
    let resolved = chrono::Local
        .with_ymd_and_hms(
            year,
            u32::from(month),
            u32::from(day),
            u32::from(hour),
            u32::from(minute),
            0,
        )
        .single()
        .expect("an unambiguous local wall time");
    time::OffsetDateTime::from_unix_timestamp(resolved.timestamp()).unwrap()
}

#[test]
fn the_presets_mean_what_their_expansions_mean() {
    // the presets are mapped before croner sees them, so the expression
    // is kept as it was given; the firings are what the expansion says
    for (preset, expanded) in [
        ("@hourly", "0 * * * *"),
        ("@daily", "0 0 * * *"),
        ("@weekly", "0 0 * * 0"),
    ] {
        assert_eq!(
            next(preset, datetime!(2026-10-28 10:07:32 UTC)),
            next(expanded, datetime!(2026-10-28 10:07:32 UTC)),
            "{preset} fires when {expanded} does"
        );
    }
}

#[test]
fn the_next_firing_is_the_next_matching_whole_minute() {
    // every 15 minutes: 10:07 with 32 seconds is followed by 10:15:00
    assert_eq!(
        next("*/15 * * * *", datetime!(2026-10-28 10:07:32 UTC)),
        datetime!(2026-10-28 10:15:00 UTC)
    );
    // a list of minutes
    assert_eq!(
        next("0,30 * * * *", datetime!(2026-10-28 10:07:00 UTC)),
        datetime!(2026-10-28 10:30:00 UTC)
    );
    // the same minute strictly after: 10:30:00 is followed by 11:30:00
    assert_eq!(
        next("30 * * * *", datetime!(2026-10-28 10:30:00 UTC)),
        datetime!(2026-10-28 11:30:00 UTC)
    );
    // steps and lists together
    assert_eq!(
        next("*/15,45 * * * *", datetime!(2026-10-28 10:07:32 UTC)),
        datetime!(2026-10-28 10:15:00 UTC)
    );
}

#[test]
fn hourly_and_daily_roll_over_midnight() {
    assert_eq!(
        next("0 * * * *", datetime!(2026-10-28 10:07:00 UTC)),
        datetime!(2026-10-28 11:00:00 UTC)
    );
    // daily at local midnight: from 10:07 UTC the next one is the next
    // day's local midnight, whatever offset that date keeps
    use chrono::TimeZone;
    let after = datetime!(2026-10-28 10:07:00 UTC);
    let local_date = chrono::DateTime::from_timestamp(after.unix_timestamp(), 0)
        .unwrap()
        .with_timezone(&chrono::Local)
        .date_naive()
        .succ_opt()
        .unwrap();
    let expected = chrono::Local
        .from_local_datetime(&local_date.and_hms_opt(0, 0, 0).unwrap())
        .single()
        .unwrap();
    assert_eq!(
        next("@daily", after),
        time::OffsetDateTime::from_unix_timestamp(expected.timestamp()).unwrap()
    );
}

#[test]
fn a_short_expression_pads_the_missing_fields_with_star() {
    // the tooltip's shorthand: */15 alone means every 15 minutes
    assert_eq!(
        next("*/15", datetime!(2026-10-28 10:07:32 UTC)),
        datetime!(2026-10-28 10:15:00 UTC)
    );
    // two fields: minute and hour — daily at 04:30 local
    assert_eq!(
        next("30 4", datetime!(2026-10-28 10:07:00 UTC)),
        local_wall(2026, 10, 29, 4, 30)
    );
    // three fields: the first of every month at 09:00 local
    assert_eq!(
        next("0 9 1", datetime!(2026-10-28 10:07:00 UTC)),
        local_wall(2026, 11, 1, 9, 0)
    );
    // four fields: every December 25th at midnight local
    assert_eq!(
        next("0 0 25 12", datetime!(2026-10-28 10:07:00 UTC)),
        local_wall(2026, 12, 25, 0, 0)
    );
}

#[test]
fn day_of_month_and_day_of_week_are_ored_when_both_are_given() {
    // the 13th, or every Friday
    let schedule = parse("0 0 13 * 5");
    // 2026-11-13 is a Friday: one match, not two
    assert_eq!(
        schedule.next_after(datetime!(2026-11-12 12:00:00 UTC)),
        Some(local_wall(2026, 11, 13, 0, 0))
    );
    // 2026-11-20 is also a Friday
    assert_eq!(
        schedule.next_after(datetime!(2026-11-14 12:00:00 UTC)),
        Some(local_wall(2026, 11, 20, 0, 0))
    );
}

#[test]
fn sunday_is_zero_and_seven() {
    // 2026-11-08 is a Sunday, and clear of both the American and the
    // European clock changes, so the local offset is the one this
    // machine keeps now
    assert_eq!(
        next("0 12 * * 7", datetime!(2026-11-02 10:00:00 UTC)),
        local_wall(2026, 11, 8, 12, 0)
    );
    assert_eq!(
        next("0 12 * * 0", datetime!(2026-11-02 10:00:00 UTC)),
        local_wall(2026, 11, 8, 12, 0)
    );
}

#[test]
fn february_the_29th_waits_for_a_leap_year() {
    assert_eq!(
        next("0 0 29 2 *", datetime!(2026-10-28 10:00:00 UTC)),
        local_wall(2028, 2, 29, 0, 0)
    );
}

#[test]
fn the_month_field_skips_whole_months() {
    assert_eq!(
        next("0 0 1 1 *", datetime!(2026-10-28 10:00:00 UTC)),
        local_wall(2027, 1, 1, 0, 0)
    );
}

#[test]
fn impossible_schedules_are_refused() {
    for text in [
        "",
        "* * * * * *",
        "61 * * * *",
        "* 24 * * *",
        "* * 0 * *",
        "* * 32 * *",
        "* * * 13 *",
        "* * * * 8",
        "*/0 * * * *",
        "a * * * *",
        "5-1 * * * *",
    ] {
        assert!(Schedule::parse(text).is_err(), "{text:?} should not parse");
    }
}

#[test]
fn the_expression_is_kept_for_the_log_lines() {
    assert_eq!(parse("*/15 * * * *").expression(), "*/15 * * * *");
    assert_eq!(parse(" @daily ").expression(), "@daily");
}

#[test]
fn a_firing_in_the_last_window_is_due_once() {
    let schedule = parse("*/15 * * * *");
    // polls every second: at 10:14:59 and 10:15:00, the 10:15 firing is
    // in the window (10:14:59, 10:15:00], and in no other
    assert!(schedule.due_in_window(
        datetime!(2026-10-28 10:14:59 UTC),
        datetime!(2026-10-28 10:15:00 UTC)
    ));
    // ...and not again in the windows that follow
    assert!(!schedule.due_in_window(
        datetime!(2026-10-28 10:15:00 UTC),
        datetime!(2026-10-28 10:15:01 UTC)
    ));
    assert!(!schedule.due_in_window(
        datetime!(2026-10-28 10:15:01 UTC),
        datetime!(2026-10-28 10:15:02 UTC)
    ));
    // the window that covers 10:30 is due again
    assert!(schedule.due_in_window(
        datetime!(2026-10-28 10:29:59 UTC),
        datetime!(2026-10-28 10:30:00 UTC)
    ));
    // a schedule whose firing is still ahead is not due
    assert!(!schedule.due_in_window(
        datetime!(2026-10-28 10:16:00 UTC),
        datetime!(2026-10-28 10:16:01 UTC)
    ));
}

#[test]
fn a_run_scheduled_in_the_spring_forward_gap_fires_after_the_gap() {
    use chrono::TimeZone;
    // 2026-03-08 is a spring-forward morning in the American timezones
    // (the clocks jump 02:00 -> 03:00) and an ordinary day elsewhere.
    // Either way, a run scheduled at 02:30 local fires at the first
    // wall-clock minute that time actually exists at.
    let schedule = parse("30 2 * * *");
    let after = chrono::Local
        .with_ymd_and_hms(2026, 3, 8, 1, 30, 0)
        .single()
        .unwrap();
    let next = schedule
        .next_after(time::OffsetDateTime::from_unix_timestamp(after.timestamp()).unwrap())
        .expect("fires");
    let next_chrono = chrono::DateTime::from_timestamp(next.unix_timestamp(), 0)
        .unwrap()
        .with_timezone(&chrono::Local);
    // in a timezone with the gap, 02:30 never came, and the firing
    // lands in the first minute after it; elsewhere 02:30 simply exists
    let expected = match chrono::Local.with_ymd_and_hms(2026, 3, 8, 2, 30, 0) {
        chrono::LocalResult::None => (3, 0), // the gap: 03:00:00 is first
        chrono::LocalResult::Single(_) => (2, 30),
        chrono::LocalResult::Ambiguous(_, _) => (2, 30),
    };
    assert_eq!(
        next_chrono,
        chrono::Local
            .with_ymd_and_hms(2026, 3, 8, expected.0, expected.1, 0)
            .single()
            .expect("the firing's wall time exists")
    );
}
