//! FediFetcher, Rust port.
//!
//! Pulls missing posts and profiles into your Mastodon instance, from the
//! accounts your users follow, reply to, list, bookmark, favourite, or get
//! notified about. Faithful port of the Python original (v8.2.3), with an
//! optional server-rendered web UI.

pub mod api;
pub mod backfill;
pub mod config;
pub mod context;
pub mod db;
pub mod debug;
pub mod error;
pub mod http;
pub mod lists;
pub mod logging;
pub mod posts;
pub mod robots;
pub mod run;
pub mod servers;
pub mod state;
pub mod store;
pub mod tasks;
pub mod timeutil;
pub mod translate;
pub mod urls;
pub mod users;
pub mod web;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
