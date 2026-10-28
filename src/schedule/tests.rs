//! The tests for `schedule.rs`. Kept in a child module so they see the
//! private items; the parent holds only the `mod tests;` pointer.

use super::*;
use time::macros::datetime;

fn parse(text: &str) -> Schedule {
    Schedule::parse(text).expect(text)
}

fn next(expression: &str, after: OffsetDateTime) -> OffsetDateTime {
    parse(expression)
        .next_after(after)
        .expect("the schedule fires within five years")
}

#[test]
fn the_presets_mean_what_their_names_say() {
    // the expression is kept as it was given, so the log lines say @daily;
    // the fields are what the preset expands to
    for (preset, expanded) in [
        ("@hourly", "0 * * * *"),
        ("@daily", "0 0 * * *"),
        ("@weekly", "0 0 * * 0"),
    ] {
        let preset = parse(preset);
        let expanded = parse(expanded);
        assert_eq!(preset.minutes, expanded.minutes, "{preset:?}");
        assert_eq!(preset.hours, expanded.hours, "{preset:?}");
        assert_eq!(preset.days, expanded.days, "{preset:?}");
        assert_eq!(preset.months, expanded.months, "{preset:?}");
    }
}

#[test]
fn every_field_form_expands() {
    let schedule = parse("*/15,45 9-17 1,15 * 1");
    assert_eq!(
        schedule.minutes,
        BTreeSet::from([0, 15, 30, 45]),
        "the step stops at the field's end, and the extra 45 is already there"
    );
    assert_eq!(
        schedule.hours,
        BTreeSet::from([9, 10, 11, 12, 13, 14, 15, 16, 17])
    );
    assert_eq!(
        schedule.months,
        BTreeSet::from([1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12])
    );
    // the day-of-week is Monday, and the day-of-month is OR'd with it
    assert_eq!(
        schedule.days,
        Days::AnyOf {
            of_month: BTreeSet::from([1, 15]),
            of_week: BTreeSet::from([1]),
        }
    );
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
}

#[test]
fn hourly_and_daily_roll_over_midnight() {
    assert_eq!(
        next("0 * * * *", datetime!(2026-10-28 10:07:00 UTC)),
        datetime!(2026-10-28 11:00:00 UTC)
    );
    assert_eq!(
        next("@daily", datetime!(2026-10-28 10:07:00 UTC)),
        datetime!(2026-10-29 00:00:00 UTC)
    );
}

#[test]
fn day_of_month_and_day_of_week_are_ored_when_both_are_given() {
    // the 13th, or every Friday
    let schedule = parse("0 0 13 * 5");
    // 2026-11-13 is a Friday: one match, not two
    assert_eq!(
        schedule.next_after(datetime!(2026-11-12 12:00:00 UTC)),
        Some(datetime!(2026-11-13 00:00:00 UTC))
    );
    // 2026-11-20 is also a Friday
    assert_eq!(
        schedule.next_after(datetime!(2026-11-14 12:00:00 UTC)),
        Some(datetime!(2026-11-20 00:00:00 UTC))
    );
}

#[test]
fn sunday_is_zero_and_seven() {
    // 2026-11-01 is a Sunday
    assert_eq!(
        next("0 12 * * 7", datetime!(2026-10-28 10:00:00 UTC)),
        datetime!(2026-11-01 12:00:00 UTC)
    );
    assert_eq!(
        next("0 12 * * 0", datetime!(2026-10-28 10:00:00 UTC)),
        datetime!(2026-11-01 12:00:00 UTC)
    );
}

#[test]
fn february_the_29th_waits_for_a_leap_year() {
    assert_eq!(
        next("0 0 29 2 *", datetime!(2026-10-28 10:00:00 UTC)),
        datetime!(2028-02-29 00:00:00 UTC)
    );
}

#[test]
fn the_month_field_skips_whole_months() {
    assert_eq!(
        next("0 0 1 1 *", datetime!(2026-10-28 10:00:00 UTC)),
        datetime!(2027-01-01 00:00:00 UTC)
    );
    // the new year's offset carries over from the search's start
    assert_eq!(
        next("0 0 1 1 *", datetime!(2026-12-31 23:30:00 UTC)),
        datetime!(2027-01-01 00:00:00 UTC)
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
        "@yearly-is-not-a-preset",
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
    // polls every 5 seconds, say at 10:14:56 and 10:15:01: the 10:15:00
    // firing is in the window (10:14:56, 10:15:01], and in no other
    assert!(schedule.due_in_window(
        datetime!(2026-10-28 10:14:56 UTC),
        datetime!(2026-10-28 10:15:01 UTC)
    ));
    // ...and not again in the windows that follow
    assert!(!schedule.due_in_window(
        datetime!(2026-10-28 10:15:01 UTC),
        datetime!(2026-10-28 10:15:06 UTC)
    ));
    assert!(!schedule.due_in_window(
        datetime!(2026-10-28 10:15:06 UTC),
        datetime!(2026-10-28 10:15:11 UTC)
    ));
    // the window that covers 10:30 is due again
    assert!(schedule.due_in_window(
        datetime!(2026-10-28 10:29:56 UTC),
        datetime!(2026-10-28 10:30:01 UTC)
    ));
    // a schedule whose firing is still ahead is not due
    assert!(!schedule.due_in_window(
        datetime!(2026-10-28 10:16:01 UTC),
        datetime!(2026-10-28 10:16:06 UTC)
    ));
}

#[test]
fn a_short_expression_pads_the_missing_fields_with_star() {
    // the tooltip's shorthand: */15 alone means every 15 minutes
    let schedule = parse("*/15");
    assert_eq!(schedule.minutes, BTreeSet::from([0, 15, 30, 45]));
    assert_eq!(schedule.hours, (0..24).collect::<BTreeSet<u8>>());
    // two fields: minute and hour — daily at 04:30
    let schedule = parse("30 4");
    assert_eq!(schedule.hours, BTreeSet::from([4]));
    // three fields: day-of-month too
    let schedule = parse("0 9 1");
    assert_eq!(schedule.days, Days::OfMonth(BTreeSet::from([1])));
    // four fields: the month too
    let schedule = parse("0 0 25 12");
    assert_eq!(schedule.months, BTreeSet::from([12]));
}
