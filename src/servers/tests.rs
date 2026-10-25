//! The tests for `servers.rs`. Kept in a child module so they see the
//! private items; the parent holds only the `mod tests;` pointer.

use super::*;

#[test]
fn every_known_software_name_maps_to_its_flavor() {
    const FLAVORS: [ApiFlavor; 4] = [
        ApiFlavor::Mastodon,
        ApiFlavor::Misskey,
        ApiFlavor::Lemmy,
        ApiFlavor::PeerTube,
    ];
    for flavor in FLAVORS {
        for name in software_apis(flavor) {
            let nodeinfo = serde_json::json!({
                "software": {"name": name, "version": "1"},
                "protocols": ["activitypub"],
            });
            let info = ServerInfo::from_nodeinfo("example.com", &nodeinfo)
                .unwrap_or_else(|| panic!("{name} should give a ServerInfo"));
            assert!(
                info.supports(flavor),
                "{name} should be detected as {}",
                flavor.as_str()
            );
            for other in FLAVORS {
                if other != flavor {
                    assert!(
                        !info.supports(other),
                        "{name} should not be detected as {}",
                        other.as_str()
                    );
                }
            }
        }
    }
}

#[test]
fn nodeinfo_maps_software_to_flavors() {
    let nodeinfo = serde_json::json!({
        "software": {"name": "mastodon", "version": "4.3.0"},
        "protocols": ["activitypub"],
    });
    let info = ServerInfo::from_nodeinfo("example.com", &nodeinfo).unwrap();
    assert!(info.supports(ApiFlavor::Mastodon));
    assert!(!info.supports(ApiFlavor::PeerTube));

    let nodeinfo = serde_json::json!({
        "software": {"name": "sharkey", "version": "2024"},
        "protocols": ["activitypub"],
    });
    let info = ServerInfo::from_nodeinfo("example.com", &nodeinfo).unwrap();
    assert!(info.supports(ApiFlavor::Misskey));
}

#[test]
fn mastodon_api_feature_adds_mastodon_flavor() {
    let nodeinfo = serde_json::json!({
        "software": {"name": "something-new", "version": "1"},
        "protocols": ["activitypub"],
        "metadata": {"features": ["mastodon_api"]},
    });
    let info = ServerInfo::from_nodeinfo("example.com", &nodeinfo).unwrap();
    assert!(info.supports(ApiFlavor::Mastodon));
}

#[test]
fn state_round_trips() {
    let nodeinfo = serde_json::json!({
        "software": {"name": "lemmy", "version": "0.19"},
        "protocols": ["activitypub"],
    });
    let info = ServerInfo::from_nodeinfo("lemmy.example", &nodeinfo).unwrap();
    let state = info.to_state();
    assert_eq!(state["lemmyApiSupport"], serde_json::json!(true));
    assert_eq!(state["peertubeApiSupport"], serde_json::json!(false));
    let back = ServerInfo::from_state(&state).unwrap();
    assert_eq!(back.webserver, "lemmy.example");
    assert!(back.supports(ApiFlavor::Lemmy));
}

#[test]
fn failed_lookup_state_reads_as_none_and_keeps_the_reason() {
    let state = failed_lookup_state("prohibited by robots.txt");
    assert!(ServerInfo::from_state(&state).is_none());
    assert_eq!(
        state["error"],
        serde_json::json!("prohibited by robots.txt")
    );
    assert!(ServerInfo::from_state(&serde_json::json!({"foo": 1})).is_none());
}

#[test]
fn host_rows_round_trip_through_columns() {
    let nodeinfo = serde_json::json!({
        "software": {"name": "mastodon", "version": "4.3.0"},
        "protocols": ["activitypub"],
    });
    let info = ServerInfo::from_nodeinfo("remote.example", &nodeinfo).unwrap();
    let value = info.to_state();

    let row = HostRow::from_state_value("remote.example", &value);
    assert_eq!(row.host, "remote.example");
    assert_eq!(row.webserver.as_deref(), Some("remote.example"));
    assert_eq!(row.software.as_deref(), Some("mastodon"));
    assert_eq!(row.version.as_deref(), Some("4.3.0"));
    assert!(row.mastodon_api);
    assert!(!row.misskey_api);
    assert!(!row.lemmy_api);
    assert!(!row.peertube_api);
    assert!(row.raw_nodeinfo.is_some(), "the nodeinfo document is kept");
    assert_eq!(row.error, None);

    // and the row reads back as the entry it was
    let back = row.to_state_value();
    assert_eq!(back["last_checked"], value["last_checked"]);
    assert_eq!(back["rawnodeinfo"], value["rawnodeinfo"]);
    let reread = ServerInfo::from_state(&back).unwrap();
    assert_eq!(reread.software, "mastodon");
    assert_eq!(reread.version, "4.3.0");
    assert!(reread.supports(ApiFlavor::Mastodon));
}

#[test]
fn a_failed_lookup_round_trips_through_columns() {
    let value = failed_lookup_state("prohibited by robots.txt");
    let row = HostRow::from_state_value("down.example", &value);
    assert_eq!(row.webserver, None);
    assert_eq!(row.error.as_deref(), Some("prohibited by robots.txt"));
    assert!(row.last_checked.is_some());

    let back = row.to_state_value();
    assert!(
        ServerInfo::from_state(&back).is_none(),
        "still a failed lookup"
    );
    assert_eq!(back["error"], serde_json::json!("prohibited by robots.txt"));
    // the failure keeps no api support keys, so expiry drops it as before
    assert!(back.get("peertubeApiSupport").is_none());
}

#[test]
fn host_meta_finds_the_lrdd_template() {
    let host_meta = r#"<?xml version="1.0" encoding="UTF-8"?>
<XRD xmlns="http://docs.oasis-open.org/ns/xri/xrd-1.0">
  <Link rel="lrdd" template="https://web.example/webfinger?q={uri}"/>
</XRD>"#;
    assert_eq!(
        parse_host_meta(host_meta, "display.example").unwrap(),
        "web.example"
    );
}

#[test]
fn host_meta_without_an_lrdd_link_is_an_error() {
    let host_meta = r#"<?xml version="1.0" encoding="UTF-8"?>
<XRD xmlns="http://docs.oasis-open.org/ns/xri/xrd-1.0">
  <Link rel="other" template="https://web.example/other?q={uri}"/>
</XRD>"#;
    let err = parse_host_meta(host_meta, "display.example").unwrap_err();
    assert_eq!(err.to_string(), "no lrdd link");
}

#[test]
fn an_lrdd_link_without_a_template_is_an_error() {
    let host_meta = r#"<?xml version="1.0" encoding="UTF-8"?>
<XRD xmlns="http://docs.oasis-open.org/ns/xri/xrd-1.0">
  <Link rel="lrdd"/>
</XRD>"#;
    let err = parse_host_meta(host_meta, "display.example").unwrap_err();
    assert_eq!(err.to_string(), "lrdd link has no template");
}

#[test]
fn a_template_without_a_server_is_an_error() {
    let host_meta = r#"<?xml version="1.0" encoding="UTF-8"?>
<XRD xmlns="http://docs.oasis-open.org/ns/xri/xrd-1.0">
  <Link rel="lrdd" template="/webfinger?q={uri}"/>
</XRD>"#;
    let err = parse_host_meta(host_meta, "display.example").unwrap_err();
    assert_eq!(
        err.to_string(),
        "server not found in lrdd for display.example"
    );
}

#[test]
fn host_meta_that_is_not_xml_is_an_error() {
    let err = parse_host_meta("not xml at all", "display.example").unwrap_err();
    assert!(
        !err.to_string().is_empty(),
        "the parser's complaint is kept: {err}"
    );
}

#[test]
fn each_flavor_names_its_state_key_and_itself() {
    assert_eq!(ApiFlavor::Mastodon.state_key(), "mastodonApiSupport");
    assert_eq!(ApiFlavor::Misskey.state_key(), "misskeyApiSupport");
    assert_eq!(ApiFlavor::Lemmy.state_key(), "lemmyApiSupport");
    assert_eq!(ApiFlavor::PeerTube.state_key(), "peertubeApiSupport");
    assert_eq!(ApiFlavor::Mastodon.as_str(), "mastodon");
    assert_eq!(ApiFlavor::Misskey.as_str(), "misskey");
    assert_eq!(ApiFlavor::Lemmy.as_str(), "lemmy");
    assert_eq!(ApiFlavor::PeerTube.as_str(), "peertube");
}
