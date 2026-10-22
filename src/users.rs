//! The one shape an account takes, whatever server it came from.

/// An account, in the one shape FediFetcher uses.
///
/// Accounts reach us two ways: our own server lists them, or a post names
/// them as its author or in its mentions. A mention is the thinner of the
/// two and says nothing about how the account wants to be treated, so the
/// three opt-out fields default to the answer an account that never asked
/// would give.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct User {
    pub acct: String,
    pub url: String,
    /// What the server that sent us this account calls it.
    ///
    /// This is how a post says who it replied to, and it only means anything
    /// on the server it came from.
    pub id: Option<String>,
    pub note: String,
    pub indexable: bool,
    pub discoverable: bool,
}

impl User {
    pub fn new(acct: impl Into<String>, url: impl Into<String>) -> Self {
        User {
            acct: acct.into(),
            url: url.into(),
            id: None,
            note: String::new(),
            indexable: true,
            discoverable: true,
        }
    }
}
