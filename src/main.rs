//! The command line: run once (the default), or serve the web UI.
//!
//! Configuration lives in the database, edited through the web UI's config
//! page; there are no configuration flags and no `ff_*` environment
//! variables.

use std::process::ExitCode;

use clap::{Parser, Subcommand};

use fedifetcher::config::Config;
use fedifetcher::{db, logging, mem, run, schedule, web};

#[derive(Parser)]
#[command(
    name = "FediFetcher-rs",
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
    /// Run on the config's schedule, headless: no web UI, one run at a
    /// time, until the process is stopped. The schedule is the
    /// `run_schedule` config value.
    Schedule,
}

enum Mode {
    Run,
    Serve { bind: String, port: u16 },
    Schedule,
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();

    let mode = match cli.command {
        Some(Command::Run) => Mode::Run,
        Some(Command::Serve { bind, port }) => Mode::Serve { bind, port },
        Some(Command::Schedule) => Mode::Schedule,
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
        Mode::Run | Mode::Schedule => Config::from_values(stored),
        Mode::Serve { .. } => Config::from_values_lenient(stored),
    };
    let config = match config {
        Ok(config) => config,
        Err(ex) => {
            // a first run on a fresh install: the message points at the
            // web UI, where the account connects itself
            if ex.to_string().contains("You must supply") {
                eprintln!("FediFetcher is not configured yet.");
                eprintln!();
                eprintln!("  1. Start the web UI:  fedifetcher-rs serve");
                eprintln!("  2. Open http://127.0.0.1:8080/config in a browser.");
                eprintln!("  3. Connect your Mastodon account and press Run.");
                eprintln!();
                eprintln!("After that, `fedifetcher-rs run` works on its own.");
                return ExitCode::from(1);
            }
            return config_error(ex);
        }
    };

    // Every log line is written to the database, which the web UI reads for
    // the logs page and the run history.
    let log_db = match logging::setup(&config, db.clone()) {
        Ok(log_db) => log_db,
        Err(ex) => return config_error(ex),
    };
    // The state directory the database lives in, kept for the exit report:
    // the config itself is handed to the run or the web server below.
    let state_dir = config.state_dir.clone();

    match mode {
        Mode::Run => {
            let result =
                run::run_once(config, run::CancelToken::new(), run::Pause::new(), &db).await;
            mem::log_exit_usage("finished a run", db::disk_usage_at(&state_dir).as_ref());
            // wait for the log lines already received to reach the database
            log_db.shutdown().await;
            match result {
                Ok(_) => ExitCode::SUCCESS,
                Err(_) => ExitCode::from(1),
            }
        }
        Mode::Schedule => {
            // the headless scheduler runs until the process is stopped,
            // so this only returns when the process is ending
            schedule::forever(config, db).await;
            log_db.shutdown().await;
            ExitCode::SUCCESS
        }
        Mode::Serve { bind, port } => match web::serve(config, &bind, port, db).await {
            Ok(reason) => {
                mem::log_exit_usage(
                    &format!("shut down ({reason})"),
                    db::disk_usage_at(&state_dir).as_ref(),
                );
                // wait for the log lines already received to reach the database
                log_db.shutdown().await;
                ExitCode::SUCCESS
            }
            Err(ex) => {
                eprintln!("ERROR:FediFetcher:{ex}");
                ExitCode::from(1)
            }
        },
    }
}

/// Log a config problem as critical and exit 1.
fn config_error(ex: fedifetcher::error::Error) -> ExitCode {
    eprintln!("CRITICAL:FediFetcher:{ex}");
    ExitCode::from(1)
}
