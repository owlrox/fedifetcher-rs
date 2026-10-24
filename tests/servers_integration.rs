//! Server discovery over the network: a host that cannot be asked what it
//! runs is remembered as failed, with the reason.

mod common;

use fedifetcher::http::HttpClient;
use fedifetcher::servers::server_info;
use fedifetcher::state::ServerCache;

/// A server that is not there yields no info, and the failure is cached with
/// its reason so the next scan does not ask again.
#[tokio::test]
async fn an_unreachable_server_is_remembered_as_a_failed_lookup() {
    let dir = common::tmp_dir();
    let config = common::test_config(&dir, "example.social");
    let db = fedifetcher::db::open(&config).await.unwrap();
    let http = HttpClient::new(config.clone(), db.clone());
    let mut seen_hosts = ServerCache::new();

    let first = server_info("127.0.0.1:9", &mut seen_hosts, &http).await;
    let second = server_info("127.0.0.1:9", &mut seen_hosts, &http).await;

    assert!(first.is_none(), "an unreachable server yields no info");
    assert!(second.is_none(), "and the cached failure says the same");
    let failure = seen_hosts
        .get("127.0.0.1:9")
        .expect("the failure is remembered");
    let reason = failure
        .get("error")
        .and_then(serde_json::Value::as_str)
        .expect("the failure records its reason");
    assert!(
        reason.contains("Error getting host node info for 127.0.0.1:9"),
        "{reason}"
    );
}
