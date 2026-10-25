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

Every version commit also updates `CHANGELOG.md`: run `./changelog.sh`
to regenerate it from the commit history and commit the result with the
bump. The changelog holds one section per version — the write-ups of
the commits that carried it — so the commit message is the changelog
entry; there is nothing separate to write. A commit subject without the
version suffix falls back to the `Cargo.toml` committed with it, which
is how the pre-1.0.5 history (and 1.0.25, whose subject forgot the
suffix) lands in the right section.

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
- Runs record themselves in `runs` from any process — the web UI's Run
  button, the command line (cron), a manual fetch ("Manual fetch: ..."),
  and the stream listener ("Stream: home timeline") — so the pill shows
  Running whenever FediFetcher is fetching, and a run from another
  process is in the history too.

## Testing

The web integration tests spawn the real server on an ephemeral port.
Timing-sensitive tests poll every 25–50 ms for up to 100 iterations
instead of sleeping a fixed time; when touching them, run the suite a
few times to shake out flakes.

No production URL builder hardcodes a scheme: the API clients and the
server lookups take theirs from the `HttpClient` they are given, so the
tests can point the whole stack at a local http server with
`HttpClient::with_scheme(config, db, "http")`. A new lookup goes through
`http.scheme()` the same way; URL *parsers* stay https-only, because
fediverse URLs are https in the wild — test fixtures spell their URLs
https even when the mock server speaks http.

Coverage is measured with cargo-llvm-cov
(`cargo llvm-cov --summary-only`); on a machine where it cannot find
llvm-tools-preview, point `LLVM_COV` and `LLVM_PROFDATA` at the
toolchain's `lib/rustlib/<host>/bin` copies. Line coverage sits at
about 95%. The uncovered remainder is deliberate, not an accident:

- database failure fallbacks (every `Err(ex) => warn!` around a db
  call): reaching them needs a fault-injecting seam over the pool, a
  refactor not worth its cost while the fallbacks are one log line
  each;
- the `web()` startup path and `main.rs`: they need a spawned process
  and assert on process plumbing, not logic;
- environment-dependent code that cannot be reached in-process
  (`default_state_dir`'s env-var branches — `set_var` is unsafe and
  process-global, so racing parallel tests);
- defensive arms that no valid input can reach (the log sorter's
  `LogSort::Time` match arm behind an early `if`).

When a new arm lands in one of these classes, leave it uncovered and
note it here rather than writing a test that asserts on an impossible
state. Pure logic and mock-reachable branches are never an excuse:
those get tests.

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
