//! The one shape a post takes, whatever server it came from.

use time::OffsetDateTime;

use crate::users::User;

/// A post from any fediverse server, in the one shape FediFetcher uses.
///
/// Servers disagree about almost everything here: what a post is called, how
/// it says who may see it, and whether it admits to having replies. Each
/// client translates its own server's posts into this shape as they arrive,
/// so that nothing downstream has to ask who it is talking to.
#[derive(Clone, Debug)]
pub struct Post {
    pub url: String,
    pub uri: String,
    pub created_at: OffsetDateTime,
    pub is_public: bool,
    pub reblog: Option<Box<Post>>,
    pub in_reply_to_id: Option<String>,
    pub in_reply_to_account_id: Option<String>,
    pub reply_count: Option<i64>,
    pub account: Option<User>,
    pub mentions: Vec<User>,
}

impl Post {
    /// The post itself, or the one it boosts.
    pub fn original(&self) -> &Post {
        self.reblog.as_deref().unwrap_or(self)
    }

    /// Whether this post is a boost of another.
    pub fn is_boost(&self) -> bool {
        self.reblog.is_some()
    }

    /// Whether the server said anything suggesting this post has replies.
    pub fn may_have_context(&self) -> bool {
        self.in_reply_to_id.is_some() || self.reply_count.is_some()
    }
}
