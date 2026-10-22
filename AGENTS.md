# AGENTS.md

## Versioning

Increment the patch version in `Cargo.toml` with every change (a point
release): 1.0.0 was the original port of FediFetcher 8.2.3, and each
revision since has bumped it (1.0.1, 1.0.2, 1.0.3, ...). Do not reuse a
version for a new change.

The version surfaces in the web UI header, in `--version`, and in the
startup log line — all read `env!("CARGO_PKG_VERSION")` — so the bump is
how you can tell you are running the build you just changed, and not an
old one by accident.
