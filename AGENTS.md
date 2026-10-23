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

End the commit subject with the version in parentheses — `Some change
(1.0.36)` — so the message and `Cargo.toml` can be checked against each
other (`git show <commit>:Cargo.toml`).

## Style

Follow the Rust style guide (https://doc.rust-lang.org/style-guide/):
idiomatic Rust over the Python original's habits. Concretely:

- No `#[allow]` anywhere: fix the cause instead. A function with too many
  arguments takes the run `Context`, or an options struct with named
  fields — never a pile of boolean flags.
- Errors are the one `Error` enum with `#[from]` conversions, not
  `String`s; use `?` at every site.
- User-facing text is US English ("favorite", "flavor"). Mastodon's own
  API names (`favourites_count`, `/api/v1/favourites`) keep their
  spelling — they are the server's, not ours.
- The web UI is server-rendered and JavaScript-free by design: Leptos
  SSR, `<meta http-equiv="refresh">` for live updates, and a cookie for
  the one preference (live updates on or off). Never add a `<script>`.

Before committing, `cargo test`, `cargo clippy --all-targets`, and
`cargo fmt --check` must all pass.

## Secrets

The database (`fedifetcher.db` in the state directory) holds the real
access token. It and its WAL side files are listed in `.gitignore` and must
never be committed. A `config.toml` or `config.json` left over from earlier
versions is no longer read, but may still hold a token, so those stay
ignored too. Before committing, check `git status --porcelain` for them,
and never run `git add -A`.
