# Running and configuring FediFetcher-rs

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
| Windows | [NSSM] or [WinSW] wrapping `fedifetcher-rs serve` | Task Scheduler: `schtasks /create /sc minute /mo 15 /tn FediFetcher /tr "...\\fedifetcher-rs.exe schedule"` |

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
