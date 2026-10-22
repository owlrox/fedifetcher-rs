//! Post URLs and the servers they point at.

use serde::{Deserialize, Serialize};

/// A post identified by the server hosting it and its ID on that server.
///
/// Serializes as a two-element array, `[server, post_id]`, which is the shape
/// the Python original writes into `replied_toot_server_ids`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct PostRef(pub String, pub String);

impl PostRef {
    pub fn new(server: impl Into<String>, post_id: impl Into<String>) -> Self {
        PostRef(server.into(), post_id.into())
    }
}

impl Serialize for PostRef {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        (&self.0, &self.1).serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for PostRef {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let (server, post_id) = <(String, String)>::deserialize(deserializer)?;
        Ok(PostRef(server, post_id))
    }
}

/// The server a URL points at, whatever the rest of it looks like.
///
/// Which server it is decides how to read the rest, so this is the one thing
/// that can be worked out without knowing what software is answering.
pub fn host_of(url: &str) -> Option<String> {
    let rest = url.strip_prefix("https://")?;
    let netloc = rest
        .split(['/', '?', '#'])
        .next()
        .unwrap_or_default();
    if netloc.is_empty() {
        None
    } else {
        Some(netloc.to_string())
    }
}

/// The `netloc` of a URL: what `urllib.parse.urlparse(url).netloc` returns.
pub fn netloc_of(url: &str) -> String {
    match url.find("://") {
        Some(scheme_end) => {
            let rest = &url[scheme_end + 3..];
            rest.split(['/', '?', '#']).next().unwrap_or_default().to_string()
        }
        None => url.split(['/', '?', '#']).next().unwrap_or_default().to_string(),
    }
}

/// The scheme of a URL: what `urllib.parse.urlparse(url).scheme` returns.
pub fn scheme_of(url: &str) -> Option<&str> {
    let scheme_end = url.find("://")?;
    let scheme = &url[..scheme_end];
    if scheme.is_empty() {
        None
    } else {
        Some(scheme)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_of_reads_https_netloc() {
        assert_eq!(
            host_of("https://mastodon.social/@user/123"),
            Some("mastodon.social".to_string())
        );
        assert_eq!(host_of("https://host:8080/x"), Some("host:8080".to_string()));
        assert_eq!(host_of("https://host"), Some("host".to_string()));
        assert_eq!(host_of("http://host/x"), None);
        assert_eq!(host_of("https://"), None);
        assert_eq!(host_of("not a url"), None);
    }

    #[test]
    fn post_ref_serializes_as_array() {
        let post = PostRef::new("example.com", "42");
        assert_eq!(serde_json::to_string(&post).unwrap(), r#"["example.com","42"]"#);
        let back: PostRef = serde_json::from_str(r#"["example.com","42"]"#).unwrap();
        assert_eq!(back, post);
    }
}
