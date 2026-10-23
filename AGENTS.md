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

Every version increment is committed together with a write-up of the
change it belongs to: bump the number, describe what changed, and commit
them in the same commit. Never push a version bump without its write-up,
and never commit a change without its bump.

## Secrets

The database (`fedifetcher.db` in the state directory) holds the real
access token. It and its WAL side files are listed in `.gitignore` and must
never be committed. A `config.toml` or `config.json` left over from earlier
versions is no longer read, but may still hold a token, so those stay
ignored too. Before committing, check `git status --porcelain` for them,
and never run `git add -A`.
