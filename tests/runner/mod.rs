//! Shared helpers for the integration tests that call the fetch functions
//! directly, through a run `Context`.

use fedifetcher::api::mastodon::HomeServer;
use fedifetcher::config::Config;
use fedifetcher::http::HttpClient;
use fedifetcher::run::{CancelToken, Context, Pause};
use fedifetcher::store::State;

/// A run context over the given pieces, for tests that call the fetch
/// functions directly.
pub fn run_context<'a>(
    config: &'a Config,
    state: &'a mut State,
    http: &'a HttpClient,
    home: &'a HomeServer,
    db: &'a sqlx::SqlitePool,
) -> Context<'a> {
    Context {
        config,
        state,
        http,
        home,
        cancel: CancelToken::new(),
        pause: Pause::new(),
        db,
    }
}
