//! The API key owner's lists of accounts.

/// One of the API key owner's lists of accounts.
///
/// A list is a name and the accounts in it. We never read the accounts from
/// the list itself: they are fetched by id, as are the posts of everyone in
/// it, so all we keep is what those two requests need.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UserList {
    pub id: String,
    /// What the owner called this list, which only ever reaches a log line.
    pub title: String,
}
