# AGENTS.md

## Versioning

Increment the version in `Cargo.toml` with every change:

- Bump the patch number (`x.y.z` to `x.y.z+1`, e.g. `1.1.3` to `1.1.4`)
  for bug fixes and small features.
- Bump the minor number (`x.y.z` to `x.(y+1).0`, e.g. `1.1.4` to
  `1.2.0`) for major or substantial changes and features, or when
  specifically requested.

Do not reuse a version for a new change.

The version surfaces in the web UI header (as a link to the about
page), in `--version`, and in the startup log line — all read
`env!("CARGO_PKG_VERSION")` — so the bump is how you can tell you are
running the build you just changed, and not an old one by accident.

Every version increment is committed together with a write-up of the
change it belongs to: bump the number, describe what changed, and commit
them in the same commit. Never commit a version bump without its
write-up, and never commit a change without its bump.

End the commit subject with the version in parentheses — `Some change
(1.1.4)` — so the message and `Cargo.toml` can be checked against each
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

## Web UI behavior

- Run status is single-sourced from the `runs` table: the newest row
  with outcome "running" decides whether the top bar shows Running,
  Paused, or Idle. There is no in-memory status to keep in sync.
- The pause flag is the one piece of runtime state kept in memory; it
  is a live signal, not persisted.
- The dashboard meta-refreshes while a run row is "running", even with
  live updates toggled off, so the status and state counts update on
  their own.
- Manual fetches are recorded in `runs` ("Manual fetch: ..."), and a
  stopped run logs the elapsed time ("Stopped after ...").

## Testing

The web integration tests spawn the real server on an ephemeral port.
Timing-sensitive tests poll every 25–50 ms for up to 100 iterations
instead of sleeping a fixed time; when touching them, run the suite a
few times to shake out flakes.

## License

The project is GPL-3.0-only: `license = "GPL-3.0-only"` in `Cargo.toml`,
the full text in `LICENSE`. It is a Rust port of the MIT-licensed
FediFetcher (https://github.com/nanos/FediFetcher, copyright Michael
Thomas); the upstream copyright and MIT notice are kept in `NOTICE.md`,
as the MIT license requires. Do not reintroduce MIT, and do not add
per-file license headers. The web UI's about page (`/about`) restates
the license and the warranty disclaimer.

## Secrets

The database (`fedifetcher.db` in the state directory) holds the real
access token. It and its WAL side files are listed in `.gitignore` and must
never be committed. A `config.toml` or `config.json` left over from earlier
versions is no longer read, but may still hold a token, so those stay
ignored too. Before committing, check `git status --porcelain` for them,
and never run `git add -A`.
