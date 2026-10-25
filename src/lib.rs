//! FediFetcher: pull missing posts and profiles into your Mastodon instance.
//!
//! Fetches from the accounts your users follow, reply to, list, bookmark,
//! favorite, or get notified about. Originally ported from the Python
//! FediFetcher (v8.2.3), with an optional server-rendered web UI.

pub mod api;
pub mod backfill;
pub mod config;
pub mod context;
pub mod db;
pub mod debug;
pub mod error;
pub mod fetch;
pub mod http;
pub mod lists;
pub mod logging;
pub mod oauth;
pub mod posts;
pub mod robots;
pub mod run;
pub mod servers;
pub mod state;
pub mod store;
pub mod stream;
pub mod tasks;
pub mod timeutil;
pub mod translate;
pub mod urls;
pub mod users;
pub mod web;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
