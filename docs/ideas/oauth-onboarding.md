# OAuth Connect for Instance Authorization

## Problem Statement

How might a fresh FediFetcher install go from zero to a working,
correctly-scoped token without the user ever opening their instance's
developer-settings page or copying a secret by hand?

## Recommended Direction

An account-management section on the existing `/config` page (direction
A), built around the existing `access_tokens` field. Every step is a
form, link, or redirect — the no-JavaScript constraint holds
throughout; the only new cookie is one short-lived state cookie beside
the existing preference cookie.

1. **Connect section** above the token field: server name + **Connect**
   button.
2. **Dynamic app registration** per server (`POST /api/v1/apps`) —
   mandatory, since client_id/secret are per-instance — cached in the
   db keyed by server, registered once and reused.
3. **303 redirect** to the instance's `/oauth/authorize` with
   `scope=read:search read:statuses admin:read:accounts` and
   `redirect_uri` derived from `Host`/`X-Forwarded-Host` *identically*
   at registration and callback time.
4. User approves on their instance; instance redirects to our
   `/oauth/callback?code=…` (with `state` checked against the cookie).
5. `POST /oauth/token` → token → `verify_credentials` answers **which
   account** this is → row added to `access_tokens`.
6. **Account list**: acct + avatar per row, Disconnect (revokes via
   `/oauth/revoke`, removes from config), and a Reconnect prompt when a
   run's request for that account comes back 401.
7. Manual paste field stays, unchanged, below.

One consent screen carries all three scopes at once (decided):
`admin:read:accounts` looks scary on a consent screen, and the copy
must explain it in plain words instead of staging a second
authorization round.

## Key Assumptions to Validate

- [x] Public origin derivable consistently from proxy headers at both
      moments — validated with forwarded headers in the oauth suite
      (both ends of the dance name the public origin); a real
      reverse-proxy recheck remains a deploy-time step
- [x] The README's three scopes are exactly what the APIs need —
      validated in live rounds, and found one short: the dance itself
      needs `read:accounts` for verify_credentials; the ask carries four
- [x] `/api/v1/apps` registration is open on target servers — validated
      live on a stock Mastodon instance; wafrn-style gated layers remain
      out of scope

## MVP Scope

Connect section, per-server registration cache, callback + exchange,
account list with disconnect/revoke, 401-reconnect prompt, manual field
untouched, httpmock e2e tests for the full dance (the `with_scheme`
machinery from 1.2.9 makes this testable), no-`<script>` assertion on
the new markup.

## Not Doing (and Why)

- **Scope staging** — one consent screen, confirmed; staging doubles
  auth churn for optics
- **Onboarding-first wizard (direction B)** — routes and db state laid
  out so it layers on later
- **Pairing-code flow for cron** — cron installs run `serve` once to
  connect; the token works in both modes
- **Refresh tokens** — Mastodon tokens don't expire by default; the
  401 prompt is the expiry story
- **Non-Mastodon OAuth (wafrn…)** — blocked upstream, documented in
  `servers.rs`

## Open Questions

- Proxy headers vs explicit public-URL field for deployed installs —
  validate first, decide after
