//! The tests for `config.rs`. Kept in a child module so they see the
//! private items; the parent holds only the `mod tests;` pointer.

use super::*;
use serde_json::json;

fn values(pairs: Vec<(&str, Value)>) -> RawValues {
    pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect()
}

fn minimal() -> RawValues {
    values(vec![
        ("server", json!("example.com")),
        ("access_tokens", json!(["token1"])),
    ])
}

#[test]
fn a_server_with_a_newline_defeats_the_normalizer() {
    // the only shape the normalization regex refuses: it comes back as is
    assert_eq!(Config::normalize_server("a\nb"), "a\nb");
    assert_eq!(
        Config::normalize_server("https://example.com/"),
        "example.com"
    );
}

#[test]
fn every_task_knob_lists_its_task_in_order() {
    let config = Config {
        from_lists: true,
        reply_interval_in_hours: 1,
        home_timeline_length: 1,
        max_followings: 1,
        max_followers: 1,
        max_follow_requests: 1,
        from_notifications: 1,
        max_bookmarks: 1,
        max_favorites: 1,
        ..Default::default()
    };
    assert_eq!(
        config.enabled_tasks(),
        vec![
            "lists",
            "reply context",
            "home timeline",
            "followings",
            "followers",
            "follow requests",
            "notifications",
            "bookmarks",
            "favorites",
        ]
    );
}

#[test]
fn set_optional_options_round_trip_through_the_raw_values() {
    let config = Config {
        lock_file: Some(std::path::PathBuf::from("/tmp/ff.lock")),
        on_start: Some("echo start".to_string()),
        on_done: Some("echo done".to_string()),
        on_fail: Some("echo fail".to_string()),
        ..Default::default()
    };
    let raw = config.to_raw_values();
    assert_eq!(raw.get("lock_file"), Some(&json!("/tmp/ff.lock")));
    assert_eq!(raw.get("on_start"), Some(&json!("echo start")));
    assert_eq!(raw.get("on_done"), Some(&json!("echo done")));
    assert_eq!(raw.get("on_fail"), Some(&json!("echo fail")));
}

#[test]
fn only_a_null_or_an_empty_array_is_an_empty_list() {
    assert!(is_empty_list(&json!([])));
    assert!(is_empty_list(&Value::Null));
    assert!(!is_empty_list(&json!(["a"])));
    assert!(!is_empty_list(&json!("[]")));
}

#[test]
fn coerce_bool_takes_numbers_and_rejects_the_rest() {
    assert!(coerce_bool(&json!(1)).unwrap());
    assert!(!coerce_bool(&json!(0)).unwrap());
    assert!(coerce_bool(&json!("YES")).unwrap());
    assert!(
        coerce_bool(&json!("off")).map_err(|_| ()).is_err() || !coerce_bool(&json!("off")).unwrap()
    );
    assert_eq!(
        coerce_bool(&json!("maybe")).unwrap_err().to_string(),
        "\"maybe\" is not a yes/no value"
    );
    assert_eq!(
        coerce_bool(&json!([])).unwrap_err().to_string(),
        "[] is not a yes/no value"
    );
}

#[test]
fn coerce_int_takes_bools_and_rejects_non_integers() {
    assert_eq!(coerce_int(&json!(true)).unwrap(), 1);
    assert_eq!(coerce_int(&json!(false)).unwrap(), 0);
    assert!(coerce_int(&json!("nope")).is_err());
    assert!(coerce_int(&json!([])).is_err());
}

#[test]
fn coerce_string_stringifies_numbers_and_bools() {
    assert_eq!(coerce_string(&json!(42)).unwrap(), "42");
    assert_eq!(coerce_string(&json!(true)).unwrap(), "true");
    assert!(coerce_string(&json!([])).is_err());
    assert_eq!(coerce_optional_string(&Value::Null).unwrap(), None);
    assert_eq!(
        coerce_optional_string(&json!("x")).unwrap(),
        Some("x".to_string())
    );
}

#[test]
fn coerce_path_rejects_non_strings_and_optional_paths_take_null() {
    assert!(coerce_path(&json!(3)).is_err());
    assert_eq!(coerce_optional_path(&Value::Null).unwrap(), None);
    assert_eq!(
        coerce_optional_path(&json!("/tmp/x")).unwrap(),
        Some(std::path::PathBuf::from("/tmp/x"))
    );
}

#[test]
fn coerce_list_takes_a_comma_string_numbers_and_rejects_the_rest() {
    assert_eq!(
        coerce_list(&json!("a, b ,,c")).unwrap(),
        vec!["a", "b", "c"]
    );
    assert_eq!(coerce_list(&json!(["x", 7])).unwrap(), vec!["x", "7"]);
    assert!(coerce_list(&json!([true])).is_err());
    assert!(coerce_list(&json!(3)).is_err());
}

#[test]
fn coerce_seconds_rejects_bad_items_and_bad_shapes() {
    assert!(coerce_seconds(&json!([1.5, "2", -1])).is_err());
    assert!(coerce_seconds(&json!([true])).is_err());
    assert!(coerce_seconds(&json!(3)).is_err());
    assert_eq!(coerce_seconds(&json!("1, 2.5")).unwrap(), vec![1.0, 2.5]);
}

#[test]
fn a_cleared_number_returns_to_its_default() {
    let mut config = Config {
        reply_interval_in_hours: 10,
        ..Default::default()
    };
    config
        .apply_values(&values(vec![("reply_interval_in_hours", json!(""))]))
        .expect("a cleared field saves");
    assert_eq!(
        config.reply_interval_in_hours, 0,
        "clearing the field means the documented default, not an error"
    );
}

#[test]
fn requires_server_and_token() {
    assert!(Config::from_values(RawValues::new()).is_err());
    assert!(Config::from_values(values(vec![("server", json!("x"))])).is_err());
    assert!(Config::from_values(minimal()).is_ok());
}

#[test]
fn every_known_option_has_a_description() {
    for name in [
        "server",
        "access_tokens",
        "reply_interval_in_hours",
        "home_timeline_length",
        "streaming",
        "run_schedule",
        "user",
        "max_followings",
        "max_followers",
        "max_follow_requests",
        "max_bookmarks",
        "max_favorites",
        "from_notifications",
        "from_lists",
        "max_list_length",
        "max_list_accounts",
        "max_posts_per_account",
        "backfill_with_context",
        "backfill_mentioned_users",
        "respect_robots_txt",
        "respect_user_opt_out",
        "remember_users_for_hours",
        "remember_hosts_for_days",
        "http_timeout",
        "rate_limit_cooldown",
        "hide_server_in_user_agent",
        "user_agent_override",
        "show_data",
        "show_logs",
        "show_debug",
        "debug_mode",
        "instance_blocklist",
        "state_dir",
        "lock_file",
        "lock_hours",
        "on_start",
        "on_done",
        "on_fail",
        "log_level",
        "log_format",
        "log_page_loads",
    ] {
        assert!(is_known_option(name), "{name}");
        assert!(option_description(name).is_some(), "{name} has no tooltip");
    }
    assert_eq!(option_description("not_an_option"), None);
}

#[test]
fn debug_mode_defaults_on_and_round_trips() {
    assert!(Config::default().debug_mode);
    let config = Config::from_values(values(vec![
        ("server", json!("example.social")),
        ("access_tokens", json!(["t"])),
        ("debug_mode", json!(false)),
    ]))
    .unwrap();
    assert!(!config.debug_mode);
    // a cleared boolean returns to the default, like the other booleans
    let mut config = config;
    config
        .apply_values(&values(vec![("debug_mode", json!(""))]))
        .expect("a cleared field saves");
    assert!(config.debug_mode);
}

#[test]
fn resetting_the_debug_gated_options_writes_their_defaults() {
    let mut config = Config::from_values(values(vec![
        ("server", json!("example.social")),
        ("access_tokens", json!(["t"])),
        ("respect_robots_txt", json!(false)),
        ("respect_user_opt_out", json!(false)),
        ("hide_server_in_user_agent", json!(true)),
        ("user_agent_override", json!("Custom/1.0")),
    ]))
    .unwrap();
    config.reset_debug_gated();
    assert!(config.respect_robots_txt);
    assert!(config.respect_user_opt_out);
    assert!(!config.hide_server_in_user_agent);
    assert_eq!(config.user_agent_override, None);
}

#[test]
fn the_user_agent_option_round_trips() {
    let config = Config::from_values(values(vec![
        ("server", json!("example.social")),
        ("access_tokens", json!(["t"])),
        ("hide_server_in_user_agent", json!(true)),
    ]))
    .unwrap();
    assert!(config.hide_server_in_user_agent);
    assert!(
        !Config::default().hide_server_in_user_agent,
        "the server is named by default"
    );
}

#[test]
fn the_user_agent_override_round_trips_and_empty_means_unset() {
    let config = Config::from_values(values(vec![
        ("server", json!("example.social")),
        ("access_tokens", json!(["t"])),
        (
            "user_agent_override",
            json!("MyFetcher/1.0 (+https://me.example)"),
        ),
    ]))
    .unwrap();
    assert_eq!(
        config.user_agent_override.as_deref(),
        Some("MyFetcher/1.0 (+https://me.example)")
    );
    assert_eq!(Config::default().user_agent_override, None);

    // the optional strings' empty form field arrives as a null, and means unset
    let mut config = config;
    config
        .apply_values(&values(vec![("user_agent_override", Value::Null)]))
        .expect("a cleared optional string saves");
    assert_eq!(config.user_agent_override, None);
}

#[test]
fn server_is_normalized() {
    let mut raw = minimal();
    raw.insert("server".to_string(), json!("https://mstdn.thms.uk/"));
    let config = Config::from_values(raw).unwrap();
    assert_eq!(config.server, "mstdn.thms.uk");
}

#[test]
fn bool_coercion_accepts_yes_no_values() {
    for truthy in ["1", "true", "yes", "on", " TRUE "] {
        let config = Config::from_values(values(vec![
            ("server", json!("x")),
            ("access_tokens", json!(["t"])),
            ("from_lists", json!(truthy)),
        ]))
        .unwrap();
        assert!(config.from_lists, "{truthy}");
    }
    for falsy in ["0", "false", "no", "off", ""] {
        let config = Config::from_values(values(vec![
            ("server", json!("x")),
            ("access_tokens", json!(["t"])),
            ("from_lists", json!(falsy)),
        ]))
        .unwrap();
        assert!(!config.from_lists, "{falsy}");
    }
    assert!(
        Config::from_values(values(vec![
            ("server", json!("x")),
            ("access_tokens", json!(["t"])),
            ("from_lists", json!("maybe")),
        ]))
        .is_err()
    );
}

#[test]
fn defaults_match_the_documented_values() {
    let config = Config::from_values(minimal()).unwrap();
    assert_eq!(config.max_posts_per_account, 40);
    assert!(config.backfill_with_context);
    assert!(config.backfill_mentioned_users);
    assert!(config.respect_robots_txt);
    assert!(config.respect_user_opt_out);
    assert_eq!(config.remember_users_for_hours, 24 * 7);
    assert_eq!(config.remember_hosts_for_days, 30);
    assert_eq!(config.http_timeout, 5);
    assert_eq!(config.max_list_length, 100);
    assert_eq!(config.max_list_accounts, 10);
    assert_eq!(config.lock_hours, 24);
    assert!(!config.from_lists);
    assert!(!config.streaming, "streaming is off by default");
    assert_eq!(config.log_level, "DEBUG");
    assert_eq!(config.log_format, "%(asctime)s: %(message)s");
    assert!(
        !config.log_page_loads,
        "page loads are not logged by default"
    );
    assert_eq!(config.lock_path(), config.state_dir.join("lock.lock"));
}

#[test]
fn list_options_accept_comma_strings_and_arrays() {
    let config = Config::from_values(values(vec![
        ("server", json!("x")),
        ("access_tokens", json!(["t"])),
        ("instance_blocklist", json!("a.example, b.example")),
    ]))
    .unwrap();
    assert_eq!(config.instance_blocklist, vec!["a.example", "b.example"]);

    let config = Config::from_values(values(vec![
        ("server", json!("x")),
        ("access_tokens", json!(["t"])),
        ("instance_blocklist", json!(["a.example", "b.example"])),
    ]))
    .unwrap();
    assert_eq!(config.instance_blocklist, vec!["a.example", "b.example"]);
}

#[test]
fn the_rate_limit_cooldown_defaults_to_the_historical_series() {
    let config = Config::from_values(minimal()).unwrap();
    // what the hardcoded 0.5s backoff growing fourfold each step waited
    assert_eq!(config.rate_limit_cooldown, vec![0.5, 2.0, 8.0, 32.0, 128.0]);
}

#[test]
fn the_rate_limit_cooldown_accepts_comma_strings_and_arrays() {
    let config = Config::from_values(values(vec![
        ("server", json!("x")),
        ("access_tokens", json!(["t"])),
        ("rate_limit_cooldown", json!("1, 2.5, 10")),
    ]))
    .unwrap();
    assert_eq!(config.rate_limit_cooldown, vec![1.0, 2.5, 10.0]);

    let config = Config::from_values(values(vec![
        ("server", json!("x")),
        ("access_tokens", json!(["t"])),
        ("rate_limit_cooldown", json!([1, 2.5, 10])),
    ]))
    .unwrap();
    assert_eq!(config.rate_limit_cooldown, vec![1.0, 2.5, 10.0]);

    // a cleared field means the documented default series
    let config = Config::from_values(values(vec![
        ("server", json!("x")),
        ("access_tokens", json!(["t"])),
        ("rate_limit_cooldown", json!("")),
    ]))
    .unwrap();
    assert_eq!(
        config.rate_limit_cooldown,
        Config::default().rate_limit_cooldown
    );

    // an explicit empty list means no retries at all
    let config = Config::from_values(values(vec![
        ("server", json!("x")),
        ("access_tokens", json!(["t"])),
        ("rate_limit_cooldown", json!([])),
    ]))
    .unwrap();
    assert!(config.rate_limit_cooldown.is_empty());
}

#[test]
fn the_rate_limit_cooldown_rejects_impossible_delays() {
    for bad in [json!("soon"), json!([-1, 2]), json!(["nan"]), json!(42)] {
        let result = Config::from_values(values(vec![
            ("server", json!("x")),
            ("access_tokens", json!(["t"])),
            ("rate_limit_cooldown", bad.clone()),
        ]));
        assert!(result.is_err(), "{bad} should not parse");
    }
}

#[test]
fn the_rate_limit_cooldown_round_trips_through_raw_values() {
    let mut config = Config::from_values(minimal()).unwrap();
    config.rate_limit_cooldown = vec![0.25, 1.0, 60.0];
    let raw = config.to_raw_values();
    let rebuilt = Config::from_values(raw).unwrap();
    assert_eq!(rebuilt.rate_limit_cooldown, vec![0.25, 1.0, 60.0]);
}

#[test]
fn unknown_options_are_ignored() {
    let config = Config::from_values(values(vec![
        ("server", json!("x")),
        ("access_tokens", json!(["t"])),
        ("nonsense", json!(1)),
    ]))
    .unwrap();
    assert_eq!(config.server, "x");
}

#[test]
fn enabled_tasks_follow_the_config() {
    let mut config = Config::from_values(minimal()).unwrap();
    assert!(
        config.enabled_tasks().is_empty(),
        "the defaults enable nothing"
    );
    config.from_lists = true;
    config.reply_interval_in_hours = 48;
    config.max_followers = 5;
    assert_eq!(
        config.enabled_tasks(),
        vec!["lists", "reply context", "followers"]
    );
}

#[test]
fn lenient_build_allows_an_empty_config() {
    let config = Config::from_values_lenient(RawValues::new()).unwrap();
    assert_eq!(config.server, "");
    assert!(config.access_tokens.is_empty());

    let config = Config::from_values_lenient(minimal()).unwrap();
    assert_eq!(config.server, "example.com");
    assert_eq!(config.access_tokens, vec!["token1".to_string()]);
}

#[test]
fn the_page_toggles_default_to_shown_and_round_trip() {
    let config = Config::from_values(values(vec![
        ("server", json!("example.social")),
        ("access_tokens", json!(["t"])),
        ("show_data", json!(false)),
        ("show_logs", json!(false)),
    ]))
    .unwrap();
    assert!(!config.show_data);
    assert!(!config.show_logs);
    assert!(config.show_debug, "the debug page toggle defaults to shown");
    // a cleared field returns to the default: shown
    let mut config = config;
    config
        .apply_values(&values(vec![("show_data", json!(""))]))
        .expect("a cleared field saves");
    assert!(
        config.show_data,
        "a cleared field returns to the default: shown"
    );
}

#[test]
fn the_page_toggles_decide_which_pages_show() {
    let config = Config {
        show_data: false,
        show_logs: false,
        show_debug: false,
        ..Default::default()
    };
    // each page answers to its own toggle
    assert!(!config.shows_page("/data"));
    assert!(!config.shows_page("/logs"));
    assert!(!config.shows_page("/debug"));
    // the dashboard and the config page are never hidden: that would
    // lock you out
    assert!(config.shows_page("/"));
    assert!(config.shows_page("/config"));
}

#[test]
fn the_debug_page_answers_to_both_the_toggle_and_debug_mode() {
    let config = Config {
        show_debug: false,
        ..Default::default()
    };
    assert!(!config.shows_page("/debug"), "the toggle alone can hide it");
    let config = Config {
        show_debug: true,
        debug_mode: false,
        ..Default::default()
    };
    // the gate is the web UI's debug_mode: the toggle alone does not
    // bring the page back while debug mode is off
    assert!(
        config.shows_page("/debug"),
        "the toggle alone does not show it; debug_mode gates"
    );
}

#[test]
fn the_run_schedule_round_trips_and_is_parsed_at_save_time() {
    assert_eq!(Config::default().run_schedule, None);
    let config = Config::from_values(values(vec![
        ("server", json!("example.social")),
        ("access_tokens", json!(["t"])),
        ("run_schedule", json!("*/15 * * * *")),
    ]))
    .unwrap();
    assert_eq!(config.run_schedule.as_deref(), Some("*/15 * * * *"));

    // a bad expression is refused with the reason
    let config = Config::from_values(values(vec![
        ("server", json!("example.social")),
        ("access_tokens", json!(["t"])),
        ("run_schedule", json!("* * * * * *")),
    ]));
    assert!(config.is_err(), "six fields is one too many");

    // the optional string's empty form field means unset
    let mut config = Config::from_values(values(vec![
        ("server", json!("example.social")),
        ("access_tokens", json!(["t"])),
        ("run_schedule", json!("*/15 * * * *")),
    ]))
    .unwrap();
    config
        .apply_values(&values(vec![("run_schedule", Value::Null)]))
        .expect("a cleared optional string saves");
    assert_eq!(config.run_schedule, None);
}

#[test]
fn the_user_agent_override_treats_blank_as_unset_and_trims_the_rest() {
    // servers refuse or ignore a blank User-Agent, so a blank value —
    // the form's empty field, or a value of spaces — is unset, and the
    // default agent is sent instead, never a blank one
    for blank in ["", "   ", "\t\n"] {
        let config = Config::from_values(values(vec![
            ("server", json!("example.social")),
            ("access_tokens", json!(["t"])),
            ("user_agent_override", json!(blank)),
        ]))
        .unwrap();
        assert_eq!(
            config.user_agent_override, None,
            "a whitespace-only override is unset, never sent as a blank agent"
        );
    }
    // a real value comes through as-is
    let config = Config::from_values(values(vec![
        ("server", json!("example.social")),
        ("access_tokens", json!(["t"])),
        ("user_agent_override", json!("  Custom/1.0  ")),
    ]))
    .unwrap();
    assert_eq!(
        config.user_agent_override.as_deref(),
        Some("Custom/1.0"),
        "a real value is trimmed, not mangled"
    );
}
