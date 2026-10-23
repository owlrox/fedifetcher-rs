//! The command line: run once (the default), or serve the web UI.
//!
//! Configuration lives in the database, edited through the web UI's config
//! page; there are no configuration flags and no `ff_*` environment
//! variables.

use std::process::ExitCode;

use clap::{Parser, Subcommand};

use fedifetcher::config::{Config, build_config, build_config_lenient};
use fedifetcher::{db, logging, run, web};

#[derive(Parser)]
#[command(
    name = "fedifetcher",
    version,
    about = "Pull missing posts and profiles into your Mastodon instance",
    long_about = "Pull missing posts and profiles into your Mastodon instance.\n\n\
        With no subcommand, runs once. Configuration lives in the database, \
        edited through the web UI's config page."
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Run FediFetcher once, then exit (the default when no subcommand is given)
    Run,
    /// Serve the web UI (dashboard, run trigger, hosts, logs, config)
    Serve {
        /// The address to listen on
        #[arg(long = "bind", default_value = "127.0.0.1")]
        bind: String,
        /// The port to listen on
        #[arg(long = "port", default_value_t = 8080)]
        port: u16,
    },
}

enum Mode {
    Run,
    Serve { bind: String, port: u16 },
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();

    let mode = match cli.command {
        Some(Command::Run) => Mode::Run,
        Some(Command::Serve { bind, port }) => Mode::Serve { bind, port },
        None => Mode::Run,
    };

    // The database lives in the default state directory and holds the
    // configuration; the web UI's config page edits it there.
    let db = match db::open(&Config::default()).await {
        Ok(db) => db,
        Err(ex) => {
            eprintln!("CRITICAL:FediFetcher:{ex}");
            return ExitCode::from(1);
        }
    };
    let stored = match db::load_config(&db).await {
        Ok(values) => values,
        Err(ex) => {
            eprintln!("CRITICAL:FediFetcher:{ex}");
            return ExitCode::from(1);
        }
    };

    // Serving only needs a config the page can edit; running needs a server
    // and an access token.
    let config = match &mode {
        Mode::Run => build_config(stored),
        Mode::Serve { .. } => build_config_lenient(stored),
    };
    let config = match config {
        Ok(config) => config,
        Err(ex) => return config_error(ex),
    };

    if let Err(ex) = logging::setup_logging(&config) {
        return config_error(ex);
    }

    // Every log line is written to the database, which the web UI reads for
    // the logs page and the run history.
    if let Err(ex) = logging::setup_log_db(db.clone()).await {
        eprintln!("CRITICAL:FediFetcher:{ex}");
        return ExitCode::from(1);
    }

    match mode {
        Mode::Run => {
            let result = run::run_once(config, run::CancelToken::new(), &db).await;
            // wait for the log lines already received to reach the database
            logging::shutdown_log_db().await;
            match result {
                Ok(_) => ExitCode::SUCCESS,
                Err(_) => ExitCode::from(1),
            }
        }
        Mode::Serve { bind, port } => {
            match web::serve(config, &bind, port, db).await {
                Ok(()) => ExitCode::SUCCESS,
                Err(ex) => {
                    eprintln!("ERROR:FediFetcher:{ex}");
                    ExitCode::from(1)
                }
            }
        }
    }
}

/// The Python original logs a config problem as critical and exits 1.
fn config_error(ex: fedifetcher::error::ConfigError) -> ExitCode {
    eprintln!("CRITICAL:FediFetcher:{ex}");
    ExitCode::from(1)
}
