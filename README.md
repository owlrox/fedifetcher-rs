# FediFetcher-rs

[![License: GPL-3.0-only](https://img.shields.io/badge/license-GPL--3.0--only-blue)](LICENSE)
[![Rust](https://img.shields.io/badge/language-Rust-orange)](https://www.rust-lang.org/)

Pull missing posts and profiles into your Mastodon instance: the context of what your users reply to, boost, bookmark, favorite, list, and get notified about, fetched from the servers that host it.

FediFetcher-rs is a Rust port of the Python [FediFetcher](https://github.com/nanos/FediFetcher) (v8.2.3), with a server-rendered web UI that needs no JavaScript.

This fork exists purely to make FediFetcher easier to run. The Python original wants a Docker image to configure, environment variables to spell correctly, API keys to create and paste by hand, and Python dependencies to keep updated; this port does away with all of that. It builds to a single binary with no runtime dependencies, the web UI walks you through connecting your account (no manual key wrangling), and the whole configuration lives in one SQLite database you can back up.

## Features

- Fetches remote replies to posts your users reply to, boost, bookmark, or favorite
- Backfills profiles and posts for new accounts your users follow, are followed by, or have pending follow requests with
- Fetches from your lists and notifications
- Streams the home timeline in serve mode (opt-in): new posts' context is fetched as they arrive, not at the next run
- Respects `robots.txt` and user opt-outs (nobot tag, not indexable or discoverable)
- Server-rendered, JavaScript-free web UI: dashboard, run/pause/stop controls, run history, logs, configuration, and state inspection
- All state — configuration, seen users and posts, run history, logs — in one SQLite database
- A single binary; TLS via rustls and SQLite compiled in, so no system OpenSSL or SQLite is needed

## Requirements

- A recent stable [Rust](https://rustup.rs/) toolchain (Rust 1.99.1+, edition 2024)
- A C and C++ compiler and cmake (for the bundled SQLite and the rustls crypto backend)

## Building from source

```sh
git clone https://github.com/owlrox/fedifetcher-rs.git
cd fedifetcher-rs
cargo build --release
```

The executable is at `target/release/fedifetcher-rs`. To install it onto your `PATH`:

```sh
cargo install --path .
```

Run the test suite with:

```sh
cargo test
```

## Deployment

For a fresh server or LXC container — Alpine, Arch, Debian, or Fedora — `deploy.sh` does the whole setup: it detects the package manager (apk, pacman, apt, or dnf/yum), installs the build dependencies, fetches the source into `/opt/fedifetcher`, installs a Rust toolchain via rustup if the system one is older than the `rust-version` in `Cargo.toml`, builds with `cargo build --release --locked`, installs the binary to `/usr/local/bin/fedifetcher-rs`, creates a `fedifetcher` system user, and sets up a service that runs the web UI (systemd where available, OpenRC on Alpine without systemd).

```sh
sudo ./deploy.sh https://github.com/owlrox/fedifetcher-rs.git [branch]   # branch defaults to main
```

The script uses the distribution's own Rust toolchain when it is new enough and falls back to rustup when it is not. Distro Rust versions as of this writing:

| Distribution | Rust shipped | Toolchain used |
| --- | --- | --- |
| Alpine Edge | 1.99.1+ | system |
| Arch | 1.99.1+ | system |
| Fedora Rawhide | 1.99.1+ | system |
| openSUSE Tumbleweed | 1.99.0 | rustup |
| Debian 13 | 1.85.1 | rustup |
| Debian 14 | 1.97.1 | rustup |

Stable Alpine and Fedora releases ship older Rust and get the rustup toolchain. Homebrew on macOS also ships a new-enough Rust for building from source.

The database lands in `/var/lib/fedifetcher/artifacts` — back it up. The web UI listens on `127.0.0.1:8080` by default; override the listen address with `FEDIFETCHER_BIND`/`FEDIFETCHER_PORT`, the branch with `FEDIFETCHER_BRANCH`, and the init system with `FEDIFETCHER_INIT=systemd|openrc|none`. `./deploy.sh --help` lists every override. Re-running the script upgrades in place: it fetches, rebuilds, and reinstalls, and restarts the service only when the binary or its unit changed — a no-change run restarts nothing. After the first run, open `/config` in the web UI to set your server and access tokens, then press **Run**.

To update a server by hand instead:

```sh
cd /opt/fedifetcher
git fetch origin && git reset --hard origin/main
cargo build --release --locked
sudo cp target/release/fedifetcher-rs /usr/local/bin/fedifetcher-rs
sudo systemctl restart fedifetcher    # or: sudo rc-service fedifetcher restart
```

The build needs a C and C++ compiler and cmake, not just a C compiler: the bundled SQLite is compiled in, and rustls's crypto backend (aws-lc-sys) is built with cmake. When running the binary by hand, `cd /var/lib/fedifetcher` first so it finds the same database the service uses. The usual run-only deployment is still the cron job described under [Scheduling regular runs](#scheduling-regular-runs).

## Running

FediFetcher has two modes: `run` performs one pass and exits, `serve` runs the web UI. With no subcommand, it runs once.

### Start the web UI

```sh
./target/release/fedifetcher-rs serve --bind 127.0.0.1 --port 8080
```

Then open <http://127.0.0.1:8080>.

First-run setup — the easy way, no token to copy:

1. Open the **Config** page (`/config`) in the web UI and type your server's name under **Connected accounts**, e.g. `mstdn.thms.uk`.
2. Press **Connect your Mastodon account**. FediFetcher registers itself on your instance and sends you to its own authorization page; approve it there and you land back on the config page with the account connected. The scopes it asks for are `read:search`, `read:statuses`, `admin:read:accounts`, and `read:accounts` (the last one is only how it learns which account it just connected). Connected accounts are listed on the page with a **Disconnect** button — which revokes the token on your instance — and a **Reconnect** button appears on any account whose token your instance has stopped accepting.
3. Choose the tasks you want to run (see [Configuration](#configuration)), then press **Run** on the dashboard (`/`).

The manual way, unchanged — for headless installs, or instances that restrict OAuth applications:

1. Create an application for your account at `https://<your-server>/settings/applications`. The access token needs the scopes the tasks you enable ask for — `read:search` and `read:statuses` always, plus `read:lists` (from_lists), `read:notifications` (from_notifications), `read:bookmarks` (max_bookmarks), `read:favourites` (max_favorites), `read:follows` (the followings/followers backfills), and `admin:read:accounts` (reply_interval_in_hours). A token missing a scope gets a 403 naming it.
2. Open the **Config** page (`/config`) in the web UI and fill in:
   - **server** — your server's name, e.g. `mstdn.thms.uk`
   - **access_tokens** — one token per user whose timelines you fetch
   - the tasks you want to run (see [Configuration](#configuration))
3. Save, then press **Run** on the dashboard (`/`).

Configuration lives in the database and is edited through the web UI — there is no config file, no environment variables, and no command-line flags. Every save logs what it changed (tokens by masked hint only), and the OAuth lifecycle — connected, disconnected, reconnect prompts — is on the Logs page like the run history is.

### Run once from the command line

```sh
./target/release/fedifetcher-rs run
```

This is also the default when no subcommand is given:

```sh
./target/release/fedifetcher-rs
```

A run takes a lock, performs the configured tasks, and exits. The exit status is 0 on success, 1 on failure. Without a configured server and access token, it exits 1 immediately with a `CRITICAL` message.

### Scheduling regular runs

The easy way: set the `run_schedule` config value in the web UI and let FediFetcher run itself. It takes a cron expression — five fields (minute, hour, day-of-month, month, day-of-week), or the presets `@hourly`, `@daily`, and `@weekly`:

```cron
*/15   # every 15 minutes
0 *    # hourly, on the hour
0 */6  # every six hours
30 4 * * *  # daily at 04:30
@daily      # daily at midnight
```

With `serve` running, a background tick reads the schedule and starts a run when a firing minute passes — logged as `Scheduled run: ...` in the run history, indistinguishable from a button-triggered run. If a run is already going when the schedule fires, the firing is skipped, not queued; the lock file keeps the schedule, the **Run** button, and any external cron job from doubling up. With the `schedule` subcommand instead, FediFetcher runs headless on the same schedule, no web UI:

```sh
fedifetcher-rs schedule
```

Times are read in the server's local timezone, so `@daily` means the local midnight. Both at once is fine: the lock file serializes the schedule, the **Run** button, and any external cron job — when a firing minute passes while an external run is going, the firing is skipped (logged as `Scheduled run: ... skipped`), and the schedule catches up at its next firing rather than piling failed runs into the history. So the usual setup is one or the other, but both at once also just works — e.g. external cron while you trial the built-in schedule. Daylight-saving changes are handled: a run scheduled in the spring-forward gap fires just after the gap, and `@daily` stays at midnight, wall time, on both change days.

The old way still works, if you'd rather keep the scheduling in cron:

```cron
0 * * * * /path/to/fedifetcher-rs run
```

### Running as a service

The recommended setup is `serve` under a service manager — it gives you the dashboard, the run controls, the logs, streaming, and the built-in schedule in one process. `deploy.sh` sets this up on Linux (systemd, or OpenRC on Alpine); for the rest:

| Platform | Run `serve` as a service | Run `schedule` headless |
| --- | --- | --- |
| Linux (systemd) | `deploy.sh` sets it up, or the unit below | A `systemd` timer or the cron entry above |
| Linux (OpenRC/Alpine) | `deploy.sh` handles OpenRC | An OpenRC cron or the cron entry above |
| macOS (launchd) | A `KeepAlive` `LaunchAgent`/`LaunchDaemon` plist running `fedifetcher-rs serve` | A `StartCalendarInterval` plist running `fedifetcher-rs schedule` |
| Windows | [NSSM] or [WinSW] wrapping `fedifetcher-rs serve` | Task Scheduler: `schtasks /create /sc minute /mo 15 /tn FediFetcher /tr "...\fedifetcher-rs.exe schedule"` |

A systemd unit, for the record:

```ini
[Unit]
Description=FediFetcher-rs
After=network-online.target

[Service]
User=fedifetcher
ExecStart=/usr/local/bin/fedifetcher-rs serve
WorkingDirectory=/var/lib/fedifetcher
Restart=on-failure

[Install]
WantedBy=multi-user.target
```

[NSSM]: https://nssm.cc
[WinSW]: https://github.com/winsw/winsw

Log lines go to stderr and to the database, where the web UI's **Logs** page shows them. Keep `serve` running (under systemd, Docker, or similar) if you want the dashboard, the run/pause/stop controls, and the logs — the **Run** button triggers the same run in the background. With `serve` running, the optional `streaming` setting also connects to your server's home timeline stream and fetches new posts' context as they arrive, so the interval matters less — with streaming on, `@hourly` is plenty.

### Command line

| Command | Description |
| --- | --- |
| `fedifetcher` / `fedifetcher run` | Run FediFetcher once, then exit |
| `fedifetcher serve [--bind ADDR] [--port PORT]` | Serve the web UI (default `127.0.0.1:8080`) |
| `fedifetcher schedule` | Run on the `run_schedule` config value, headless, until stopped |
| `fedifetcher --help` | Print help |
| `fedifetcher --version` | Print the version |

### Web UI

| Page | What it does |
| --- | --- |
| `/` | Dashboard: run status, state counts, run/pause/resume/stop |
| `/config` | Edit the configuration (stored in the database) |
| `/logs` | Log output and run history (`POST /logs/clear` clears them) |
| `/data` | The state database: users, posts, hosts, opted-out accounts, each with a fetch button |
| `/debug` | Test the connection to your server, look up users, fetch a user's recent posts |
| `/about` | About and license |

Behind the buttons: `POST /run`, `POST /stop`, `POST /pause`, `POST /resume` control runs, and `POST /fetch` fetches a single post, user, host, or URL on demand. `GET /healthz` is a health check for monitoring. The UI is server-rendered and JavaScript-free by design.

## Configuration

The configuration is stored in the database and edited through the web UI's config page, which shows a tooltip for every option. The defaults:

| Option | Default | Description |
| --- | --- | --- |
| `server` | *(required)* | Your server's name, e.g. `mstdn.thms.uk` |
| `access_tokens` | *(required)* | One token per user — connected through the config page's **Connect your Mastodon account** (which asks only for the scopes your enabled tasks need, plus `read:accounts` for the connection itself), or created by hand at `https://<server>/settings/applications` with the scopes the enabled tasks need (see the setup steps above) |
| `reply_interval_in_hours` | `0` (off) | Fetch remote replies to posts that received replies from users on your instance in this period |
| `home_timeline_length` | `0` (off) | Look for replies to posts in the API key owner's home timeline, up to this many posts |
| `streaming` | `false` | Stream the home timeline over a long-lived connection and fetch context for new posts as they arrive, instead of waiting for the next run. Serve mode only, with the first access token; applies on restart |
| `run_schedule` | *(empty)* | A cron expression for FediFetcher to run itself on: five fields (minute hour day-of-month month day-of-week), or `@hourly`, `@daily`, `@weekly`. Empty means no schedule. See [Scheduling regular runs](#scheduling-regular-runs) |
| `user` | *(token owner)* | Use with `max_followings`/`max_followers` to backfill a specific user's followings/followers |
| `max_followings` | `0` (off) | Backfill posts for new accounts followed by `user`, at most this many |
| `max_followers` | `0` (off) | Backfill posts for new accounts following `user`, at most this many |
| `max_follow_requests` | `0` (off) | Backfill posts of the API key owner's pending follow requests, at most this many |
| `max_bookmarks` | `0` (off) | Fetch remote replies to the API key owner's bookmarks, at most this many |
| `max_favorites` | `0` (off) | Fetch remote replies to the API key owner's favorites, at most this many |
| `from_notifications` | `0` (off) | Backfill accounts of anyone appearing in your notifications during the last hours |
| `from_lists` | `false` | Fetch missing replies and/or backfill accounts from your lists |
| `max_list_length` | `100` | Posts to fetch replies for in each list; `0` only backfills profiles |
| `max_list_accounts` | `10` | Accounts to backfill in each list; `0` only fetches replies |
| `max_posts_per_account` | `40` | Posts to fetch from each account backfilled; raising it makes runs considerably slower |
| `backfill_with_context` | `true` | Fetch remote replies when backfilling profiles |
| `backfill_mentioned_users` | `true` | Backfill mentioned users when fetching remote replies to timeline posts |
| `respect_robots_txt` | `true` | Respect `robots.txt` when fetching from remote servers |
| `respect_user_opt_out` | `true` | Skip accounts that have opted out (nobot tag, or not indexable or discoverable) when backfilling |
| `remember_users_for_hours` | `168` | How long to remember users you aren't following, before trying to backfill them again |
| `remember_hosts_for_days` | `30` | How long to remember host info, before checking again |
| `http_timeout` | `5` | Timeout in seconds for HTTP requests to your own or other instances |
| `rate_limit_cooldown` | `0.5,2,8,32,128` | Seconds to wait between retries when a server rate limits (HTTP 429), one entry per retry step; the request fails when the list runs out |
| `hide_server_in_user_agent` | `false` | Omit your server's name from the User-Agent sent to other servers |
| `user_agent_override` | *(unset)* | Send this User-Agent to other servers instead of the default one, as-is; when set, `hide_server_in_user_agent` is ignored |
| `instance_blocklist` | *(empty)* | Comma-separated list of instances to never connect to |
| `state_dir` | see below | Directory for persistent files and the lock file |
| `lock_file` | `<state_dir>/lock.lock` | Location of the lock file |
| `lock_hours` | `24` | How long a lock held by a live process is respected; locks from dead processes are reclaimed immediately |
| `on_start`, `on_done`, `on_fail` | *(unset)* | URLs pinged when processing starts, completes, or fails — for dead man's switch monitoring |
| `log_level` | `DEBUG` | Severity of events to log: `DEBUG`, `INFO`, `WARNING`, `ERROR`, or `CRITICAL`; applies on restart |
| `log_format` | `%(asctime)s: %(message)s` | The log format; applies on restart |
| `log_page_loads` | `false` | Log a line for every page load of the web UI, at `DEBUG` |
| `ui_pages` | `all` | Which pages the web UI shows: `all`, `no-debug` (Data and Logs), or `logs-only`. The config page itself is always shown |
| `debug_mode` | `true` | Show the debug tooling: the Debug page, the Data page's Actions column, and the politeness options (`respect_robots_txt`, `respect_user_opt_out`, `hide_server_in_user_agent`, `user_agent_override`). Turning it off writes those options' defaults back, so what is hidden is always the documented default |

## State and data

The state directory is `./artifacts` if that directory exists in the working directory, otherwise `$XDG_STATE_HOME/fedifetcher` (usually `~/.local/state/fedifetcher`). It holds:

- `fedifetcher.db` — the SQLite database with the configuration (including the access tokens), the state of seen users, posts, and hosts, the run history, and the logs
- `lock.lock` — the lock file that keeps two runs from overlapping

Back the database up if you care about the run history; it holds your access token, and it and its WAL side files are in `.gitignore` — never commit them.

## Development

```sh
cargo test                  # unit and integration tests; the web tests spawn the real server on an ephemeral port
cargo clippy --all-targets  # must pass before committing
cargo fmt --check           # must pass before committing
```

The layout: `src/main.rs` is the command line (`run`/`serve`), `src/lib.rs` and the modules beside it are the library (configuration, database, fetching, the run loop, the web UI), and `tests/` holds the integration tests.

## Roadmap

Ideas for future releases, drawn from the [Mastodon API documentation](https://docs.joinmastodon.org/api/) and from endpoints that exist in the [Mastodon source](https://github.com/mastodon/mastodon) but are not documented. Ordered roughly by impact; where an endpoint needs a minimum server version, it is noted, and reading `api_versions` from `GET /api/v2/instance` (itself on this list) would let each item be gated on the server's version. The unauthenticated API surface was also probed read-only against a live Mastodon 4.6.9 server (API version 11); what that confirmed is summarized in [Verified against a live server](#verified-against-a-live-server) and noted on the items it touches.

### Verified against a live server

Probed with no credentials, read-only, against a Mastodon 4.6.9 instance (API version 11):

- **Answers without a token**: `accounts/lookup` (including remote accounts, resolved through WebFinger), `accounts/:id` and `accounts/:id/statuses`, `statuses/:id` and `statuses/:id/context`, `statuses/:id/reblogged_by` and `/favourited_by`, the batch `statuses?id[]=`, `polls/:id`, `instance` and its sub-resources (including the undocumented `instance/languages`), `custom_emojis`, `tags/:name`, `trends/tags|statuses|links`, `directory`, `v2/search` without `resolve`, the undocumented `peers/search`, `health`, `streaming/health`, `oembed`, and `timelines/public?local=true` with `Link` pagination on instances whose local feed is public. `statuses/:id/history` also answers for public statuses, though it is no longer in the official docs.
- **Answers only with a token**: `v2/search` with `resolve=true` (401, *"Search queries that resolve remote resources are not supported without authentication"*), `statuses/:id/quotes`, `statuses/:id/source`, every streaming channel (401, *"Missing access token"*, even for public streams on servers that enforce it), and all personal endpoints (notifications, bookmarks, lists, filters, and so on).
- **Gated by the 4.5+ timeline access control**: the federated public, hashtag, and link timelines, and `announcements`, answer 422 (*"This method requires an authenticated user"*) without a token where `configuration.timelines_access` sets the remote feed to `authenticated`.
- **Removed in current Mastodon**: `v1/search`, `statuses/:id/card` (the card is a field of the Status JSON), and `timelines/direct` fall through to the web UI's HTML 404.
- **Other confirmed behavior**: `admin/accounts` answers 403 (not 401) without the admin scope; rate-limit headers (`X-RateLimit-Limit/Remaining/Reset`, 300 per 5 minutes) are sent even on unauthenticated responses; `instance/peers` can be very large (hundreds of KB on a small instance with many known domains), so fetch it rarely; `instance/terms_of_service` and `instance/translation/languages` 404 when unset or disabled.

### Making fetched posts actually appear in the home timeline

- [ ] Parse the search-resolve response instead of accepting any HTTP 200: a 200 with an empty `statuses` array means the resolve failed (deleted, private, or blocked post), and the response carries the resolved status's local id, which the next item needs. The resolve itself needs the token: without one the server answers 401, not a result.
- [ ] Boost missing context posts into the home timeline with `POST /api/v1/statuses/:id/reblog` (`visibility=unlisted`, to limit profile noise). Resolving through search only makes the server fetch a post; a boost guarantees it surfaces in the home timeline. Requires the `write:statuses` scope, so it would be an opt-in configuration flag, off by default.
- [ ] Read notification requests (`GET /api/v1/notifications/requests`, 4.3.0+): replies and mentions from accounts you don't follow are filtered into requests rather than `GET /api/v1/notifications`, so they are currently missed. `GET /api/v1/notifications/unread_count` makes a cheap pre-check before paging.
- [ ] Follow the async refresh on context fetches (4.5.0+): `GET /api/v1/statuses/:id/context` may answer with a `Mastodon-Async-Refresh: id="...", retry=3` header while the server runs background jobs to find and fetch missing replies; polling `GET /api/v1_alpha/async_refreshes/:id` until `finished` and re-fetching context picks those replies up. Not observed in probing — a status with no replies carried no such header — but the probed server version supports it.
- [x] Stream instead of poll: a long-lived connection to `GET /api/v1/streaming/user` turns every `update` event into an immediate trigger post instead of waiting for the next run. Done in 1.1.10 as an SSE listener in serve mode behind the `streaming` option, using the first configured access token (channels answer 401 without a token on servers that enforce it), with the streaming host from `configuration.urls.streaming` in `GET /api/v2/instance` — a WebSocket URL, read over its HTTP equivalent — falling back to the API's own host. The endpoints are path-based (`/api/v1/streaming/public/local` and friends; the old `?stream=` query style is gone); `/api/v1/streaming/health` is a liveness check that answers without a token.

### More trigger sources

- [ ] Followed hashtags: `GET /api/v1/followed_tags` + `GET /api/v1/timelines/tag/:hashtag` — context for followed hashtags, including conversations no local user has touched. `followed_tags` is personal and needs a token; the tag timeline is public only where hashtag feeds are open.
- [ ] Federated timeline: `GET /api/v1/timelines/public?remote=true` as a config-gated trigger source. On servers using the 4.5+ timeline access control this answers 422 without a token, so the source only works on instances with open remote feeds or when given a token. The local feed (`local=true`) is public on more instances and pages with `Link` headers.
- [ ] Quotes: `GET /api/v1/statuses/:id/quotes` (4.5.0+) — quote posts of trigger posts are context the pipeline never looks at. Confirmed to require a token.
- [ ] Link timeline: `GET /api/v1/timelines/link?url=<url>` (4.3.0+) — other statuses sharing a trigger post's link card. Gated like the other remote feeds on access-controlled servers.
- [ ] Conversations: `GET /api/v1/conversations` — direct-message threads are never scanned.
- [ ] Boost and favorite audiences: `GET /api/v1/statuses/:id/reblogged_by` and `/favourited_by` — backfill posts of accounts who boosted or favorited trigger posts. Both answer without a token for public statuses.
- [ ] Trends: `GET /api/v1/trends/statuses|tags|links` — trending content as a trigger source; trending links feed the link timeline. All three answer without a token.

### Efficiency and correctness

- [ ] Batch status fetch: `GET /api/v1/statuses?id[]=...` (4.3.0+) — verify many trigger posts exist, or many resolved posts landed, in one request instead of one per post. Answers without a token; unknown ids simply drop out of the returned array.
- [ ] Feature detection: read `api_versions.mastodon` and `configuration` from `GET /api/v2/instance` (4.3.0+) instead of relying on nodeinfo alone, to version-gate the items above; also provides `configuration.urls.streaming`. Confirmed worthwhile: a 4.6.9 server reports API version 11 and lacks the 5.0.0 (version 12) home-timeline parameters below.
- [ ] Respect filters and blocks before resolving: `GET /api/v1/filters` (or `/api/v2/filters` with keywords), `GET /api/v1/accounts/relationships?id[]=`, `GET /api/v1/domain_blocks`, and `GET /api/v1/instance/domain_blocks` — today every public context URL is resolved, including filtered content and blocked or muted domains.
- [ ] Pace requests with the rate-limit headers: `X-RateLimit-Remaining` and `X-RateLimit-Reset` come with every response (300 requests per 5 minutes per account); pause before hitting a 429 instead of retrying after it. They are sent on unauthenticated responses too, where the limit is per IP.
- [ ] Timeline markers for notifications: `GET /api/v1/markers?timeline[]=notifications` + `POST /api/v1/markers` — fetch with `since_id=<last read id>` for exact incremental scanning instead of a time window that can miss notifications between runs.
- [ ] Validate tokens at startup: `POST /oauth/introspect` (RFC 7662, provided by Doorkeeper; not in the official docs) to fail fast when a token lacks `read:search`, `read:statuses`, or `admin:read:accounts` — or `write:statuses` if boosting is enabled.
- [ ] Smarter account scans: `GET /api/v1/accounts/:id/statuses` supports `min_id` (exact incremental scanning from state) and `exclude_reblogs` (skip boosts in the reply scan; their context is handled through the original post). Answers without a token for public statuses.
- [ ] Deeper context with an authenticated fetch, opt-in: context is limited to 40 ancestors / 60 descendants / depth 20 unauthenticated, and 4,096 / 4,096 / unlimited depth with a user token and `read:statuses`. The trade-off: remote servers see which account queried them.
- [ ] Home timeline filters (5.0.0+, API version 12): the `exclude_direct`, `exclude_reblogs`, `exclude_quotes`, and `exclude_replies` parameters to slim the home scan. Absent on API version 11, confirmed.

### Undocumented endpoints worth considering

Present in the Mastodon source (`config/routes/api.rb` and its controllers), absent from the official docs:

- [ ] `GET /api/v1/accounts/familiar_followers?id[]=` — accounts you follow who follow the given accounts; rank context posts by familiarity. Needs `read:follows` (401 without a token).
- [ ] `GET /api/v1/peers/search?q=` — search a server's known instance domains; locate the canonical server for an account. Answers without a token.
- [ ] `GET /health` — liveness check on a remote server before fetching from it. Answers without a token.

Considered and not planned: `annual_reports`, `donation_campaigns`, `collections`, `web/*`, `accounts/:id/note`, `accounts/:id/email_subscriptions`, `statuses/:id/interaction_policy`, `conversations/:id/unread`, `instance/languages`, and `domain_blocks/preview` — present in the source but not useful for fetching context. Also confirmed removed on current servers: `GET /api/v1/search`, `GET /api/v1/statuses/:id/card` (the card is a field of the Status JSON), and `GET /api/v1/timelines/direct`.

## Changelog

Versions and their write-ups are collected in [CHANGELOG.md](CHANGELOG.md), one section per version, newest first.

## License

FediFetcher-rs is free software: you can redistribute it and/or modify it under the terms of the [GNU General Public License](https://www.gnu.org/licenses/gpl-3.0.html) as published by the Free Software Foundation, either version 3 of the License, or (at your option) any later version. The full text ships as `LICENSE`.

FediFetcher-rs itself is Copyright (C) 2026 owlrox — [github.com/owlrox/fedifetcher-rs](https://github.com/owlrox/fedifetcher-rs).

It is a Rust port of the original [FediFetcher](https://github.com/nanos/FediFetcher), Copyright (C) Michael Thomas, which is MIT-licensed; the upstream copyright and MIT notice are kept in `NOTICE.md`, as the MIT license requires.
