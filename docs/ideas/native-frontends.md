# Native Frontends: TUI, iced, and GTK4

## Problem Statement

How might we let people configure and monitor FediFetcher-rs without a browser —
over SSH on a server, or locally on the desktop — while keeping the no-JavaScript
principle intact?

## Recommended Direction

All three native frontends are on the table and the working expectation is that
all three get built. They share one foundation — the core split — and are
sequenced by cost, with each one's build informed by what the previous one
taught about the core/UI boundary:

1. **Core split** — move the ~14k core lines into their own workspace crate so
   every frontend talks to the same library. Nearly free (see below), and it is
   the prerequisite for everything else.
2. **TUI (ratatui)** — first, because it is the cheapest and serves both user
   groups: server admins over SSH and desktop users who want local control
   without a browser.
3. **iced** — the pure-Rust desktop frontend. Keeps the single clean binary and
   the `deploy.sh` story untouched. Its own-widget look is an accepted trade,
   not a blocker.
4. **GTK4 (gtk4-rs)** — the native-theming frontend. It starts as a few-day
   trial building one real window; if that goes well it graduates to a full
   frontend at parity with the TUI.

The order is by cost, not importance, and nothing here is a throwaway: each
frontend is a real operator surface — configure the app, run it, read its logs.
Qt and Tauri stay out (see Not Doing); they add cost without adding a capability
the other three lack.

**Why the core split first:** `Cargo.toml` already declares a `[lib]` target, so
this is moving ~14k lines (everything except `web.rs`'s 3,137) into a separate
crate and updating `crate::` imports. Measured effect: ~5x faster recompilation
of UI-only changes (18% of the code), unchanged binary size, zero runtime or
memory cost. Three frontends sharing one core is exactly what makes building all
of them sustainable instead of a maintenance treadmill.

## What Each Frontend Reuses

Everything already exists; no frontend needs new plumbing:

- **Dashboard:** `db::LogRow` / `RunRow` for history and logs; the
  `STATE_TABLE_COUNTS` list for state and table counts.
- **Controls:** `run_once`, `Pause`, and `CancelToken` in `src/run.rs`.
- **Config editing:** `config.rs`'s `is_known_option` / `option_description` —
  the same metadata the web config form is built on.

## Key Assumptions to Validate

- [ ] Multiple frontends can share the SQLite database concurrently (SQLite
      locking handles this, but verify log/state reads don't contend) — test by
      running the TUI against a live server deployment.
- [ ] `run_once` is safe when triggered from two frontends — audit `CancelToken`
      and `Pause` semantics in `src/run.rs` for the multi-frontend case.
- [ ] Config writes from two frontends don't race — the web config page writes
      to the database; define an ordering or single-writer rule.
- [ ] The core/UI boundary is actually clean — the split itself is the test; if
      moving modules out drags web-specific code with it, that's the finding.
- [ ] Building three frontends is sustainable long-term — the shared core and
      the no-new-plumbing rule are the bet; if a frontend starts duplicating
      logic, stop and reconsider.

## First Version

1. **Core split** (mechanical): workspace crate for the ~14k core lines;
   `web.rs` becomes a bin-side module. All existing tests pass unchanged.
2. **TUI dashboard**: run history (`RunRow`), last N log lines (`LogRow` with
   sort/order), state and table counts, with a tokio select loop driving
   crossterm events and periodic refresh.
3. **TUI controls**: trigger run, pause, cancel — via the existing `Pause` /
   `CancelToken` API.
4. **TUI config editor**: form-style editing over the same option metadata the
   web page uses; writes to the database like the web page does.
5. **iced window**: the same dashboard and controls in iced, once the TUI has
   proven the core boundary.
6. **GTK4 trial**: one real window with native theming; decide afterwards
   whether it graduates to full parity.

## Not Doing (and Why)

- **Qt frontend** — duplicates GTK's value at 4–8+ weeks, adds a C++/cmake
  toolchain and LGPL licensing weight to a project whose identity is one clean
  Rust binary. Revisit only if a one-day measurement of the setup cost alone
  demands it.
- **Tauri / webview shell** — Tauri injects IPC JavaScript into every page and
  its runtime is a JS-capable webview. Breaks the no-JS principle in spirit.
- **Local JSON-RPC / `UiApi` trait abstraction** — premature until the split
  proves frontends are cheap to add; revisit if a fourth frontend ever appears.
- **Changing the web UI** — it stays the primary interface for remote
  deployments; nothing in this work touches `web.rs` behavior.

## Open Questions

- Does any native frontend need OAuth onboarding
  (`SPEC-oauth-onboarding.md`), or is first-run setup still web-only?
  (Leaning web-only; revisit after the TUI.)
- Single-writer rule for config: does a native frontend refuse to edit while
  the web UI is open, or is it last-write-wins with a warning?
- Packaging: ship the TUI in the same `fedifetcher` binary as a subcommand
  (`fedifetcher tui`) rather than a second binary — and same question for the
  GUI frontends later.
- Theme strategy for iced, given it cannot follow the system GTK/Qt theme: is
  its own look acceptable everywhere, or does iced lose the desktop slot to
  GTK4 if theming proves to matter more than build cleanliness?
