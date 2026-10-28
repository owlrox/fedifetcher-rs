//! The tests for `robots.rs`. Kept in a child module so they see the
//! private items; the parent holds only the `mod tests;` pointer.

use super::*;

fn parser(robots: &str) -> RobotFileParser {
    let mut parser = RobotFileParser::new();
    parser.parse(robots.lines().map(str::to_string));
    parser
}

const AGENT: &str = "FediFetcher/8.2.3; +example.com (+https://github.com/owlrox/fedifetcher-rs)";

#[test]
fn empty_robots_allows_everything() {
    let parser = parser("");
    assert!(parser.can_fetch(AGENT, "https://example.com/anything"));
}

#[test]
fn star_group_applies_to_our_agent() {
    let parser = parser("User-agent: *\nDisallow: /private/\n");
    assert!(parser.can_fetch(AGENT, "https://example.com/public"));
    assert!(!parser.can_fetch(AGENT, "https://example.com/private/x"));
}

#[test]
fn specific_group_beats_star() {
    let parser = parser("User-agent: *\nDisallow: /\n\nUser-agent: FediFetcher\nAllow: /\n");
    assert!(parser.can_fetch(AGENT, "https://example.com/anything"));
}

#[test]
fn empty_disallow_means_allow_all() {
    let parser = parser("User-agent: *\nDisallow:\n");
    assert!(parser.can_fetch(AGENT, "https://example.com/anything"));
}

#[test]
fn longest_rule_wins() {
    let parser = parser("User-agent: *\nDisallow: /p\nAllow: /p/public\n");
    assert!(!parser.can_fetch(AGENT, "https://example.com/private"));
    assert!(parser.can_fetch(AGENT, "https://example.com/p/public/x"));
}

#[test]
fn equal_length_rules_allow_wins() {
    let disallow_first = parser("User-agent: *\nDisallow: /a\nAllow: /a\n");
    assert!(disallow_first.can_fetch(AGENT, "https://example.com/abc"));
    let reversed = parser("User-agent: *\nAllow: /a\nDisallow: /a\n");
    assert!(reversed.can_fetch(AGENT, "https://example.com/abc"));
}

#[test]
fn dollar_anchors_the_end() {
    let parser = parser("User-agent: *\nDisallow: /x$\n");
    assert!(!parser.can_fetch(AGENT, "https://example.com/x"));
    assert!(parser.can_fetch(AGENT, "https://example.com/xy"));
}

#[test]
fn wildcard_matches_anything() {
    let parser = parser("User-agent: *\nDisallow: /*.pdf$\n");
    assert!(!parser.can_fetch(AGENT, "https://example.com/a/b.pdf"));
    assert!(parser.can_fetch(AGENT, "https://example.com/a/b.pdfx"));
}

#[test]
fn wildcard_prefix() {
    let parser = parser("User-agent: *\nDisallow: /private/*\n");
    assert!(!parser.can_fetch(AGENT, "https://example.com/private/x"));
    assert!(parser.can_fetch(AGENT, "https://example.com/public/x"));
}

#[test]
fn comments_and_case_are_ignored() {
    let parser = parser("# a comment\nUSER-AGENT: * # trailing\nDISALLOW: /x # nope\n");
    assert!(!parser.can_fetch(AGENT, "https://example.com/x"));
    assert!(parser.can_fetch(AGENT, "https://example.com/y"));
}

#[test]
fn robots_txt_itself_is_allowed() {
    let parser = parser("User-agent: *\nDisallow: /\n");
    assert!(parser.can_fetch(AGENT, "https://example.com/robots.txt"));
    assert!(!parser.can_fetch(AGENT, "https://example.com/other"));
}

#[test]
fn query_is_part_of_the_match() {
    let parser = parser("User-agent: *\nDisallow: /search\n");
    assert!(!parser.can_fetch(AGENT, "https://example.com/search?q=x"));
}

#[test]
fn percent_encoding_is_normalized() {
    let parser = parser("User-agent: *\nDisallow: /a%20b\n");
    assert!(!parser.can_fetch(AGENT, "https://example.com/a b"));
    assert!(!parser.can_fetch(AGENT, "https://example.com/a%20b"));
}

#[test]
fn agent_token_matches_by_substring() {
    let parser = parser("User-agent: fetcher\nDisallow: /\n");
    // "fetcher" is a substring of our product token "fedifetcher"
    assert!(!parser.can_fetch(AGENT, "https://example.com/x"));
}

#[test]
fn unparsed_parser_allows_nothing() {
    let parser = RobotFileParser::new();
    assert!(!parser.can_fetch(AGENT, "https://example.com/x"));
}

#[test]
fn consecutive_user_agent_lines_share_a_group() {
    let parser = parser("User-agent: a\nUser-agent: fetcher\nDisallow: /\n");
    assert!(!parser.can_fetch(AGENT, "https://example.com/x"));
}

#[test]
fn rule_before_any_user_agent_is_ignored() {
    let parser = parser("Disallow: /\nUser-agent: *\nAllow: /\n");
    assert!(parser.can_fetch(AGENT, "https://example.com/x"));
}

#[test]
fn two_groups_for_one_agent_merge_their_rules() {
    let parser = parser("User-agent: *\nDisallow: /x\n\nUser-agent: *\nDisallow: /y\n");
    assert!(!parser.can_fetch(AGENT, "https://example.com/x/1"));
    assert!(!parser.can_fetch(AGENT, "https://example.com/y/2"));
    assert!(parser.can_fetch(AGENT, "https://example.com/z"));
}

#[test]
fn lines_without_a_colon_are_skipped() {
    let parser = parser("User-agent: *\ngarbage\nDisallow: /x\n");
    assert!(!parser.can_fetch(AGENT, "https://example.com/x/1"));
    assert!(parser.can_fetch(AGENT, "https://example.com/y"));
}

#[test]
fn an_exact_agent_name_hits_its_group_directly() {
    let parser = parser("User-agent: fedi\nDisallow: /x\n");
    assert!(!parser.can_fetch("fedi", "https://example.com/x/1"));
    assert!(parser.can_fetch("fedi", "https://example.com/y"));
}

#[test]
fn the_disallow_all_and_allow_all_switches_decide_alone() {
    let mut parser = RobotFileParser::new();
    parser.disallow_all = true;
    assert!(!parser.can_fetch(AGENT, "https://example.com/robots.txt"));
    let mut parser = RobotFileParser::new();
    parser.allow_all = true;
    assert!(parser.can_fetch(AGENT, "https://example.com/private"));
}

#[test]
fn a_url_with_no_path_is_the_root() {
    let parser = parser("User-agent: *\nDisallow: /\n");
    assert!(!parser.can_fetch(AGENT, "https://example.com"));
}

#[test]
fn default_matches_new() {
    assert_eq!(
        RobotFileParser::default().can_fetch(AGENT, "https://example.com/x"),
        RobotFileParser::new().can_fetch(AGENT, "https://example.com/x")
    );
}

#[test]
fn a_dollar_mid_pattern_makes_the_rule_unusable() {
    // '$$' collapses to '$', and a '$' not at the end is rejected, so
    // the rule line is dropped and the path stays allowed
    let parser = parser("User-agent: *\nDisallow: /a$$b\n");
    assert!(parser.can_fetch(AGENT, "https://example.com/a$b"));
}

#[test]
fn a_wildcard_pattern_missing_a_middle_literal_does_not_match() {
    let parser = parser("User-agent: *\nDisallow: /a*b*c\n");
    assert!(parser.can_fetch(AGENT, "https://example.com/a/x"));
    assert!(!parser.can_fetch(AGENT, "https://example.com/a/1/b/2/c"));
}

#[test]
fn a_fullmatch_wildcard_with_an_empty_tail_matches_any_suffix() {
    let parser = parser("User-agent: *\nDisallow: /a*$\n");
    assert!(!parser.can_fetch(AGENT, "https://example.com/abc"));
}

#[test]
fn a_wildcard_pattern_missing_its_tail_literal_does_not_match() {
    let parser = parser("User-agent: *\nDisallow: /a*b\n");
    assert!(parser.can_fetch(AGENT, "https://example.com/a/x"));
    assert!(!parser.can_fetch(AGENT, "https://example.com/a/1/b"));
}

#[test]
fn a_rule_with_a_query_matches_the_query_too() {
    let parser = parser("User-agent: *\nDisallow: /p?a=b\n");
    assert!(!parser.can_fetch(AGENT, "https://example.com/p?a=b"));
    assert!(parser.can_fetch(AGENT, "https://example.com/p?c=d"));
}

#[test]
fn extract_path_query_keeps_the_query_and_drops_the_rest() {
    assert_eq!(
        extract_path_query("https://example.com/p?q=1#frag"),
        "/p?q=1"
    );
    assert_eq!(extract_path_query("/no/scheme?q"), "/no/scheme?q");
    assert_eq!(extract_path_query("https://example.com"), "");
}
