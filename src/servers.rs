//! Finding out what software a server runs, via nodeinfo, so the right API
//! client can be picked for it.

use std::collections::BTreeSet;
use std::time::Duration;

use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use time::OffsetDateTime;
use tracing::{debug, error, warn};

use crate::http::HttpClient;
use crate::state::ServerCache;
use crate::timeutil;

const NODEINFO_SCHEMAS: &[&str] = &[
    "http://nodeinfo.diaspora.software/ns/schema/2.0",
    "http://nodeinfo.diaspora.software/ns/schema/2.1",
];

const XRD_NAMESPACE: &str = "http://docs.oasis-open.org/ns/xri/xrd-1.0";

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ApiFlavour {
    Mastodon,
    Misskey,
    Lemmy,
    PeerTube,
}

impl ApiFlavour {
    /// How each flavour is spelled in the seen_hosts file.
    pub fn state_key(self) -> &'static str {
        match self {
            ApiFlavour::Mastodon => "mastodonApiSupport",
            ApiFlavour::Misskey => "misskeyApiSupport",
            ApiFlavour::Lemmy => "lemmyApiSupport",
            ApiFlavour::PeerTube => "peertubeApiSupport",
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            ApiFlavour::Mastodon => "mastodon",
            ApiFlavour::Misskey => "misskey",
            ApiFlavour::Lemmy => "lemmy",
            ApiFlavour::PeerTube => "peertube",
        }
    }
}

/// Support for new server software should be added here.
///
/// Software that has specific API support but is not compatible with
/// FediFetcher for various reasons:
/// * gotosocial - all Mastodon APIs require an access token
///   (https://github.com/superseriousbusiness/gotosocial/issues/2038)
pub fn software_apis(flavour: ApiFlavour) -> &'static [&'static str] {
    match flavour {
        ApiFlavour::Mastodon => &[
            "mastodon",
            "pleroma",
            "akkoma",
            "pixelfed",
            "hometown",
            "iceshrimp",
            "Iceshrimp.NET",
            "fedibird",
            "kmyblue",
            "mitra",
        ],
        ApiFlavour::Misskey => &[
            "misskey",
            "calckey",
            "firefish",
            "foundkey",
            "sharkey",
            "cherrypick",
        ],
        ApiFlavour::Lemmy => &["lemmy"],
        ApiFlavour::PeerTube => &["peertube"],
    }
}

/// What we know about a server, and which APIs we can talk to it with.
#[derive(Clone, Debug)]
pub struct ServerInfo {
    pub webserver: String,
    pub software: String,
    pub version: String,
    pub apis: BTreeSet<ApiFlavour>,
    pub last_checked: OffsetDateTime,
    pub raw: Value,
}

impl ServerInfo {
    pub fn supports(&self, flavour: ApiFlavour) -> bool {
        self.apis.contains(&flavour)
    }

    pub fn from_nodeinfo(webserver: &str, nodeinfo: &Value) -> Option<ServerInfo> {
        let software = nodeinfo.get("software")?.get("name")?.as_str()?;
        let version = nodeinfo
            .get("software")
            .and_then(|s| s.get("version"))
            .and_then(Value::as_str)
            .unwrap_or("");

        let mut apis = BTreeSet::new();
        for flavour in [
            ApiFlavour::Mastodon,
            ApiFlavour::Misskey,
            ApiFlavour::Lemmy,
            ApiFlavour::PeerTube,
        ] {
            if software_apis(flavour).contains(&software) {
                apis.insert(flavour);
            }
        }

        // search `features` list in metadata if available
        if let Some(features) = nodeinfo
            .get("metadata")
            .and_then(|m| m.get("features"))
            .and_then(Value::as_array)
        {
            if features.iter().any(|f| f.as_str() == Some("mastodon_api")) {
                apis.insert(ApiFlavour::Mastodon);
            }
        }

        Some(ServerInfo {
            webserver: webserver.to_string(),
            software: software.to_string(),
            version: version.to_string(),
            apis,
            last_checked: timeutil::now(),
            raw: nodeinfo.clone(),
        })
    }

    pub fn to_state(&self) -> Value {
        let mut state = Map::new();
        state.insert("webserver".to_string(), Value::String(self.webserver.clone()));
        state.insert("software".to_string(), Value::String(self.software.clone()));
        state.insert("version".to_string(), Value::String(self.version.clone()));
        state.insert("rawnodeinfo".to_string(), self.raw.clone());
        state.insert(
            "last_checked".to_string(),
            Value::String(timeutil::fmt_state_ts(self.last_checked)),
        );
        for flavour in [
            ApiFlavour::Mastodon,
            ApiFlavour::Misskey,
            ApiFlavour::Lemmy,
            ApiFlavour::PeerTube,
        ] {
            state.insert(
                flavour.state_key().to_string(),
                Value::Bool(self.apis.contains(&flavour)),
            );
        }
        Value::Object(state)
    }

    /// Read a seen_hosts entry, or None if it records a failed lookup.
    pub fn from_state(state: &Value) -> Option<ServerInfo> {
        let object = state.as_object()?;
        if object.get("info") == Some(&Value::Null) || !object.contains_key("webserver") {
            return None;
        }
        let last_checked = object
            .get("last_checked")
            .and_then(Value::as_str)
            .and_then(timeutil::parse)?;
        let mut apis = BTreeSet::new();
        for flavour in [
            ApiFlavour::Mastodon,
            ApiFlavour::Misskey,
            ApiFlavour::Lemmy,
            ApiFlavour::PeerTube,
        ] {
            if object.get(flavour.state_key()).and_then(Value::as_bool) == Some(true) {
                apis.insert(flavour);
            }
        }
        Some(ServerInfo {
            webserver: object.get("webserver")?.as_str()?.to_string(),
            software: object
                .get("software")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            version: object
                .get("version")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            apis,
            last_checked,
            raw: object.get("rawnodeinfo").cloned().unwrap_or(Value::Object(Map::new())),
        })
    }
}

/// The seen_hosts entry for a server whose lookup failed, keeping the reason
/// so that later errors can report why.
pub fn failed_lookup_state(reason: &str) -> Value {
    serde_json::json!({
        "info": null,
        "error": reason,
        "last_checked": timeutil::fmt_state_ts(timeutil::now()),
    })
}

/// One row of the seen_hosts table: a host's fields broken out of the JSON
/// the state used to keep them in. `raw_nodeinfo` stays JSON — its shape is
/// the server's own — but everything FediFetcher reads is a column.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HostRow {
    pub host: String,
    pub webserver: Option<String>,
    pub software: Option<String>,
    pub version: Option<String>,
    pub mastodon_api: bool,
    pub misskey_api: bool,
    pub lemmy_api: bool,
    pub peertube_api: bool,
    pub last_checked: Option<String>,
    /// Why the lookup failed, when it did.
    pub error: Option<String>,
    /// The nodeinfo document, as JSON text.
    pub raw_nodeinfo: Option<String>,
}

impl HostRow {
    /// The row for one seen_hosts entry, as the state spells it.
    pub fn from_state_value(host: &str, value: &Value) -> Self {
        let text = |key: &str| value.get(key).and_then(Value::as_str).map(str::to_string);
        let supported = |flavour: ApiFlavour| {
            value
                .get(flavour.state_key())
                .and_then(Value::as_bool)
                .unwrap_or(false)
        };
        HostRow {
            host: host.to_string(),
            webserver: text("webserver"),
            software: text("software"),
            version: text("version"),
            mastodon_api: supported(ApiFlavour::Mastodon),
            misskey_api: supported(ApiFlavour::Misskey),
            lemmy_api: supported(ApiFlavour::Lemmy),
            peertube_api: supported(ApiFlavour::PeerTube),
            last_checked: text("last_checked"),
            error: text("error"),
            raw_nodeinfo: value.get("rawnodeinfo").map(Value::to_string),
        }
    }

    /// The seen_hosts entry this row reads back as: the same shape
    /// `ServerInfo::to_state` and `failed_lookup_state` write, so the state
    /// in memory is unchanged by the trip through columns. A row that
    /// recorded a failed lookup — the only entries with an `error` — reads
    /// back as the failure it was; anything else keeps the fields the hosts
    /// page shows, even when it has no webserver to name.
    pub fn to_state_value(&self) -> Value {
        if self.error.is_some() {
            return serde_json::json!({
                "info": null,
                "error": self.error,
                "last_checked": self.last_checked,
            });
        }
        let mut state = Map::new();
        state.insert("webserver".to_string(), serde_json::json!(self.webserver));
        state.insert("software".to_string(), serde_json::json!(self.software));
        state.insert("version".to_string(), serde_json::json!(self.version));
        if let Some(raw) = &self.raw_nodeinfo {
            // the document as it was; one that does not parse reads as null
            let parsed = serde_json::from_str(raw).unwrap_or(Value::Null);
            state.insert("rawnodeinfo".to_string(), parsed);
        }
        state.insert(
            "last_checked".to_string(),
            serde_json::json!(self.last_checked),
        );
        for flavour in [
            ApiFlavour::Mastodon,
            ApiFlavour::Misskey,
            ApiFlavour::Lemmy,
            ApiFlavour::PeerTube,
        ] {
            state.insert(
                flavour.state_key().to_string(),
                Value::Bool(self.supported(flavour)),
            );
        }
        Value::Object(state)
    }

    fn supported(&self, flavour: ApiFlavour) -> bool {
        match flavour {
            ApiFlavour::Mastodon => self.mastodon_api,
            ApiFlavour::Misskey => self.misskey_api,
            ApiFlavour::Lemmy => self.lemmy_api,
            ApiFlavour::PeerTube => self.peertube_api,
        }
    }
}

/// The reason a cached lookup failed, if the cache records one.
fn cached_failure_reason(cache: &ServerCache, server: &str) -> Option<String> {
    cache
        .get(server)
        .and_then(|info| info.get("error"))
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// Look a server up, remembering both successes and failures. A failure is
/// remembered with the reason it failed.
pub async fn get_server_info(
    server: &str,
    cache: &mut ServerCache,
    http: &HttpClient,
) -> Option<ServerInfo> {
    if cache.contains(server) {
        return ServerInfo::from_state(cache.get(server).expect("checked contains"));
    }

    match get_nodeinfo(server, cache, false, http).await {
        Ok(info) => {
            cache.add(server, info.to_state());
            if server != info.webserver {
                cache.add(&info.webserver, info.to_state());
            }
            Some(info)
        }
        Err(reason) => {
            cache.add(server, failed_lookup_state(&reason));
            None
        }
    }
}

/// Fetch a server's nodeinfo. The error is the same message the log prints,
/// so a failure is reported with its reason wherever it is next needed.
pub async fn get_nodeinfo(
    server: &str,
    cache: &mut ServerCache,
    host_meta_fallback: bool,
    http: &HttpClient,
) -> Result<ServerInfo, String> {
    let url = format!("https://{server}/.well-known/nodeinfo");
    let resp = match http.get(&url, Some(Duration::from_secs(30))).await {
        Ok(resp) => resp,
        Err(ex) => {
            error!("Error getting host node info for {server}. Exception: {ex}");
            return Err(format!("Error getting host node info for {server}. Exception: {ex}"));
        }
    };

    // if well-known nodeinfo isn't found, try to check host-meta for a
    // webfinger URL, needed on servers where the display domain is different
    // than the web domain
    if resp.status != 200 && !host_meta_fallback {
        debug!("nodeinfo for {server} not found, checking host-meta");
        let Some(new_server) = get_server_from_host_meta(server, http).await else {
            return Err(format!(
                "no nodeinfo found for {server} (well-known returned {})",
                resp.status
            ));
        };
        if new_server == server {
            debug!("host-meta for {server} did not get a new server.");
            return Err(format!(
                "host-meta for {server} did not identify a different web server"
            ));
        }
        return Box::pin(get_nodeinfo(&new_server, cache, true, http)).await;
    }

    if resp.status != 200 {
        error!(
            "Error getting well-known host node info for {server}. Status Code: {}",
            resp.status
        );
        return Err(format!(
            "Error getting well-known host node info for {server}. Status Code: {}",
            resp.status
        ));
    }

    let node_location = match extract_node_location(&resp) {
        Ok(location) => location,
        Err(ex) => {
            error!("error getting server {server} info from well-known node info. Exception: {ex}");
            return Err(format!(
                "error getting server {server} info from well-known node info. Exception: {ex}"
            ));
        }
    };

    let Some(node_location) = node_location else {
        error!("could not find link to node info in well-known nodeinfo of {server}");
        return Err(format!(
            "could not find link to node info in well-known nodeinfo of {server}"
        ));
    };

    // regrab server from node_location, again in the case of different
    // display and web domains
    let pattern = Regex::new(r"https://(?P<server>[^/]+)/").unwrap();
    let Some(server) = pattern
        .captures(&node_location)
        .and_then(|c| c.name("server"))
        .map(|m| m.as_str().to_string())
    else {
        error!("Error getting web server name from {server}.");
        return Err(format!("Error getting web server name from {server}."));
    };

    // return early if the web domain has been seen previously (in cases with
    // host-meta lookups)
    if cache.contains(&server) {
        let state = cache.get(&server).expect("checked contains");
        return match ServerInfo::from_state(state) {
            Some(info) => Ok(info),
            None => Err(cached_failure_reason(cache, &server)
                .unwrap_or_else(|| format!("no server info for {server}"))),
        };
    }

    let resp = match http
        .get(&node_location, Some(Duration::from_secs(30)))
        .await
    {
        Ok(resp) => resp,
        Err(ex) => {
            error!("Error getting host node info for {server}. Exception: {ex}");
            return Err(format!("Error getting host node info for {server}. Exception: {ex}"));
        }
    };

    if resp.status != 200 {
        error!(
            "Error getting host node info for {server}. Status Code: {}",
            resp.status
        );
        return Err(format!(
            "Error getting host node info for {server}. Status Code: {}",
            resp.status
        ));
    }

    let nodeinfo = match resp.json() {
        Ok(nodeinfo) => nodeinfo,
        Err(ex) => {
            error!("error getting server {server} info from nodeinfo. Exception: {ex}");
            return Err(format!(
                "error getting server {server} info from nodeinfo. Exception: {ex}"
            ));
        }
    };

    let protocols = nodeinfo.get("protocols").and_then(Value::as_array);
    if !protocols
        .map(|p| p.iter().any(|x| x.as_str() == Some("activitypub")))
        .unwrap_or(false)
    {
        warn!("server {server} does not support activitypub, skipping");
        return Err(format!("server {server} does not support activitypub, skipping"));
    }

    match ServerInfo::from_nodeinfo(&server, &nodeinfo) {
        Some(info) => Ok(info),
        None => {
            error!("error getting server {server} info from nodeinfo. Exception: missing software name");
            Err(format!(
                "error getting server {server} info from nodeinfo. Exception: missing software name"
            ))
        }
    }
}

fn extract_node_location(resp: &crate::http::Response) -> Result<Option<String>, crate::error::Error> {
    let body = resp.json()?;
    let mut node_location = None;
    if let Some(links) = body.get("links").and_then(Value::as_array) {
        for link in links {
            if link.get("rel").and_then(Value::as_str).map(|rel| NODEINFO_SCHEMAS.contains(&rel)) == Some(true) {
                node_location = link.get("href").and_then(Value::as_str).map(str::to_string);
                break;
            }
        }
    }
    Ok(node_location)
}

pub async fn get_server_from_host_meta(server: &str, http: &HttpClient) -> Option<String> {
    let url = format!("https://{server}/.well-known/host-meta");
    let resp = match http.get(&url, Some(Duration::from_secs(30))).await {
        Ok(resp) => resp,
        Err(ex) => {
            error!("Error getting host meta for {server}. Exception: {ex}");
            return None;
        }
    };

    if resp.status != 200 {
        error!(
            "Error getting host meta for {server}. Status Code: {}",
            resp.status
        );
        return None;
    }

    match parse_host_meta(&resp.text, server) {
        Ok(found) => Some(found),
        Err(ex) => {
            error!("Error parsing host meta for {server}. Exception: {ex}");
            None
        }
    }
}

fn parse_host_meta(text: &str, server: &str) -> Result<String, crate::error::Error> {
    let document = roxmltree::Document::parse(text)
        .map_err(|ex| crate::error::Error(format!("{ex}")))?;
    let lrdd = document
        .descendants()
        .filter(|node| node.is_element())
        .find(|node| {
            node.tag_name().name() == "Link"
                && node.tag_name().namespace() == Some(XRD_NAMESPACE)
                && node.attribute("rel") == Some("lrdd")
        })
        .ok_or_else(|| crate::error::Error("no lrdd link".to_string()))?;
    let template = lrdd
        .attribute("template")
        .ok_or_else(|| crate::error::Error("lrdd link has no template".to_string()))?;
    let pattern = Regex::new(r"https://(?P<server>[^/]+)/").unwrap();
    match pattern.captures(template).and_then(|c| c.name("server")) {
        Some(found) => Ok(found.as_str().to_string()),
        None => Err(crate::error::Error(format!(
            "server not found in lrdd for {server}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_known_software_name_maps_to_its_flavour() {
        const FLAVOURS: [ApiFlavour; 4] = [
            ApiFlavour::Mastodon,
            ApiFlavour::Misskey,
            ApiFlavour::Lemmy,
            ApiFlavour::PeerTube,
        ];
        for flavour in FLAVOURS {
            for name in software_apis(flavour) {
                let nodeinfo = serde_json::json!({
                    "software": {"name": name, "version": "1"},
                    "protocols": ["activitypub"],
                });
                let info = ServerInfo::from_nodeinfo("example.com", &nodeinfo)
                    .unwrap_or_else(|| panic!("{name} should give a ServerInfo"));
                assert!(
                    info.supports(flavour),
                    "{name} should be detected as {}",
                    flavour.as_str()
                );
                for other in FLAVOURS {
                    if other != flavour {
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
    fn nodeinfo_maps_software_to_flavours() {
        let nodeinfo = serde_json::json!({
            "software": {"name": "mastodon", "version": "4.3.0"},
            "protocols": ["activitypub"],
        });
        let info = ServerInfo::from_nodeinfo("example.com", &nodeinfo).unwrap();
        assert!(info.supports(ApiFlavour::Mastodon));
        assert!(!info.supports(ApiFlavour::PeerTube));

        let nodeinfo = serde_json::json!({
            "software": {"name": "sharkey", "version": "2024"},
            "protocols": ["activitypub"],
        });
        let info = ServerInfo::from_nodeinfo("example.com", &nodeinfo).unwrap();
        assert!(info.supports(ApiFlavour::Misskey));
    }

    #[test]
    fn mastodon_api_feature_adds_mastodon_flavour() {
        let nodeinfo = serde_json::json!({
            "software": {"name": "something-new", "version": "1"},
            "protocols": ["activitypub"],
            "metadata": {"features": ["mastodon_api"]},
        });
        let info = ServerInfo::from_nodeinfo("example.com", &nodeinfo).unwrap();
        assert!(info.supports(ApiFlavour::Mastodon));
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
        assert!(back.supports(ApiFlavour::Lemmy));
    }

    #[test]
    fn failed_lookup_state_reads_as_none_and_keeps_the_reason() {
        let state = failed_lookup_state("prohibited by robots.txt");
        assert!(ServerInfo::from_state(&state).is_none());
        assert_eq!(state["error"], serde_json::json!("prohibited by robots.txt"));
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
        assert!(reread.supports(ApiFlavour::Mastodon));
    }

    #[test]
    fn a_failed_lookup_round_trips_through_columns() {
        let value = failed_lookup_state("prohibited by robots.txt");
        let row = HostRow::from_state_value("down.example", &value);
        assert_eq!(row.webserver, None);
        assert_eq!(row.error.as_deref(), Some("prohibited by robots.txt"));
        assert!(row.last_checked.is_some());

        let back = row.to_state_value();
        assert!(ServerInfo::from_state(&back).is_none(), "still a failed lookup");
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
}
