//! Shared helpers for the integration tests that open a database.

use fedifetcher::config::Config;

/// Open the database in the config's state directory, creating the schema.
pub async fn test_db(config: &Config) -> sqlx::SqlitePool {
    fedifetcher::db::open(config).await.unwrap()
}
