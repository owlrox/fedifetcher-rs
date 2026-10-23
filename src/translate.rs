//! What every client needs to turn what a server sent into a shape we use.
//!
//! The shapes themselves are in `posts` and `users`; the `parse_post` and
//! `parse_user` that build them live with the client that knows the dialect.
//! These are the three things all of those translations have in common.

use regex::Regex;
use serde_json::Value;
use time::OffsetDateTime;
use tracing::debug;

/// What the first of these patterns picks out of a URL, if any.
///
/// Each pattern captures the one part worth having as `name` — an account on
/// a profile URL, a post id on a post URL — so the same matching serves both.
///
/// The Python original matches with `re.match`, which anchors at the start of
/// the string; every pattern here is anchored the same way.
pub fn first_name_in(patterns: &[Regex], url: &str) -> Option<String> {
    for pattern in patterns {
        if let Some(captures) = pattern.captures(url) {
            if let Some(name) = captures.name("name") {
                return Some(name.as_str().to_string());
            }
        }
    }
    None
}

/// Compile a pattern the way `re.match` would see it: anchored at the start.
pub fn anchored(pattern: &str) -> Regex {
    let pattern = if pattern.starts_with('^') {
        pattern.to_string()
    } else {
        format!("^{pattern}")
    };
    Regex::new(&pattern).expect("invalid built-in pattern")
}

/// Keep what we could make sense of, and forget the rest.
pub fn usable<T>(things: impl IntoIterator<Item = Option<T>>) -> Vec<T> {
    things.into_iter().flatten().collect()
}

/// A string field that means something, or None: servers send "" where they
/// mean absent, and Python's `or` treats both the same way.
pub fn non_empty_str(value: Option<&Value>) -> Option<String> {
    value
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// Read whatever a server calls a date, or admit we could not.
pub fn parse_date(value: &Value) -> Option<OffsetDateTime> {
    match value {
        Value::String(text) => crate::timeutil::parse(text),
        _ => None,
    }
}

/// Something we cannot address or date is something we can do nothing with.
pub fn unusable(flavor: &str, identifier: &Value) {
    debug!("Skipping a {flavor} post without a URL or a date: {identifier}");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_name_in_picks_the_named_group() {
        let patterns = [
            anchored(r"^https://[^/]+/@(?P<name>[^/]+)"),
            anchored(r"^https://[^/]+/users/(?P<name>[^/]+)"),
        ];
        assert_eq!(
            first_name_in(&patterns, "https://example.com/@alice"),
            Some("alice".to_string())
        );
        assert_eq!(
            first_name_in(&patterns, "https://example.com/users/bob"),
            Some("bob".to_string())
        );
        assert_eq!(first_name_in(&patterns, "https://example.com/other"), None);
    }

    #[test]
    fn first_name_in_anchors_at_start() {
        // no ^ in the pattern, but re.match semantics still anchor it
        let patterns = [anchored(r"/notice/(?P<name>[^/]+)")];
        assert_eq!(
            first_name_in(&patterns, "/notice/123"),
            Some("123".to_string())
        );
        assert_eq!(first_name_in(&patterns, "https://h/notice/123"), None);
    }

    #[test]
    fn parse_date_reads_common_shapes() {
        assert!(parse_date(&serde_json::json!("2026-10-22T01:23:45.123Z")).is_some());
        assert!(parse_date(&serde_json::json!("2026-10-22 01:23:45+00:00")).is_some());
        assert!(parse_date(&serde_json::json!("not a date")).is_none());
        assert!(parse_date(&serde_json::json!(42)).is_none());
        assert!(parse_date(&serde_json::json!(null)).is_none());
    }
}
