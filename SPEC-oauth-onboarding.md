# Spec: OAuth Connect for Instance Authorization

The refined concept this spec implements is `docs/ideas/oauth-onboarding.md`
(the one-pager from the idea-refine session). One spec, two shipped-together
parts: the connect flow and the reconnect lifecycle.

## Objective

A fresh FediFetcher install goes from zero to a working, correctly-scoped
access token without the user ever opening their instance's
developer-settings page or copying a secret by hand. The web UI's config
page gains an account-management section: connect an instance account
through Mastodon's OAuth authorization-code flow, see the connected
accounts, disconnect (revoke) one, and reconnect one whose token stopped
working. The manual paste-a-token field stays, unchanged, for headless
setups and instances that restrict OAuth apps.

Who it is for: anyone who downloads FediFetcher — setup must carry its own
explanation. Success: the developer-settings page is never visited; the
config page reads as "these accounts are connected" rather than "here are
secrets you must produce".

Everything stays server-rendered with zero JavaScript: forms, links, and
303 redirects only.

## Tech Stack

As the crate already stands — axum 0.8 (serve mode's router), reqwest 0.13
with rustls, sqlx 0.9 sqlite, Leptos 0.8 SSR. No new dependencies: the
OAuth client is three HTTPS calls (register, exchange, revoke) plus one
redirect, hand-rolled with the existing `HttpClient` machinery. The
 Mastodon endpoints used:

- `POST /api/v1/apps` — dynamic app registration, per server; grants
  `client_id` + `client_secret`
- `GET /oauth/authorize` — the instance's own consent screen (we only
  build the URL and redirect)
- `POST /oauth/token` — code-for-token exchange
- `POST /oauth/revoke` — on disconnect
- `GET /api/v1/accounts/verify_credentials` — which account the token is

Scopes requested: `read:search read:statuses admin:read:accounts`, one
consent screen, with copy explaining that "admin" here means read-only
account info about the user's own accounts.

## Commands

```
Build:      cargo build --release
Test:       cargo test
Lint:       cargo clippy --all-targets -- -D warnings
Format:     cargo fmt --check
Coverage:   cargo llvm-cov --summary-only
```

Per AGENTS.md: all three gates pass before every commit, and every change
ships with its version bump and write-up.

## Project Structure

```
src/oauth.rs                  → new: the OAuth client (registration cache,
                                authorize URL builder, code exchange, revoke,
                                verify_credentials), scheme-aware like every
                                other lookup
src/web.rs                    → new routes: GET/POST connect, GET /oauth/callback,
                                POST disconnect; the account list in the config page
src/db.rs                     → new tables + migration: oauth_apps (per-server
                                client credentials), oauth_accounts (per-account
                                metadata + needs_reconnect)
tests/oauth_integration.rs    → new: the full dance against httpmock playing the
                                instance (register → authorize redirect → callback
                                → exchange → verify), denial, disconnect, reconnect
tests/web_integration.rs      → config-page tests: connect section renders, account
                                list renders, no <script> anywhere
docs/ideas/oauth-onboarding.md → the one-pager (exists)
```

## Code Style

Per AGENTS.md, which is the binding style document. In shorthand: idiomatic
Rust, one `Error` enum with `#[from]`, `?` everywhere, no `#[allow]`, US
English, no `<script>`, secrets never logged — tokens pass through the
existing `masked()` helper.

```rust
// shape of the exchange, to match the house style:
pub async fn exchange_code(
    http: &HttpClient,
    app: &RegisteredApp,
    code: &str,
    redirect_uri: &str,
) -> Result<ConnectedAccount> {
    let resp = http.post_form(&format!("{}/oauth/token", app.base), &[...]).await?;
    ...
}
```

## Testing Strategy

- `tests/oauth_integration.rs`: httpmock plays the instance. The dance is
  fully exercisable: mock `/api/v1/apps`, `/oauth/token`,
  `/accounts/verify_credentials`, `/oauth/revoke`; the authorize step is
  asserted on the redirect's Location (scheme, path, client_id, scope,
  redirect_uri, state), not by rendering someone else's consent screen.
  The `HttpClient::with_scheme` machinery (1.2.9) points the whole dance
  at http.
- `tests/web_integration.rs`: the config page's new markup — connect
  section present, account rows render acct + avatar, disconnect posts a
  303 back, and the page still contains no `<script>`.
- Timing: poll-don't-sleep per AGENTS.md. Coverage expectation: the new
  module lands above the suite's 92% line bar; run `cargo llvm-cov
  --summary-only` before the final commit.

## Boundaries

- **Always:** gates before every commit (test, clippy -D warnings, fmt);
  version bump + write-up per change; poll-don't-sleep in tests; masked
  tokens in logs and page markup; US English user-facing text.
- **Ask first:** the two new db tables (schema change); any new
  dependency; a new config field (the public-URL fallback, if proxy
  headers prove insufficient); changing the existing access_tokens
  round-trip behavior.
- **Never:** a `<script>` tag; a token or client_secret in a log line,
  page markup, or error message; committing a database file; removing the
  manual paste path.

## Success Criteria

1. On a fresh install (empty config), the config page shows the connect
   section; entering a server and pressing Connect redirects to that
   server's `/oauth/authorize` with the client_id the app just registered
   there, the three scopes, and a redirect_uri equal to the public
   origin + `/oauth/callback`.
2. Completing the round-trip (approving on the instance) adds the
   account: `access_tokens` gains the token, the account list renders the
   acct and avatar from `verify_credentials`, and the token round-trips
   through the db exactly as a manually pasted one does.
3. Denying consent returns to the config page with a message and adds
   nothing.
4. Disconnect revokes the token upstream (verified by the mock receiving
   the revoke), removes it from `access_tokens`, and drops the row.
5. A 401 on a connected account's requests during a run marks that
   account "needs reconnect"; its row offers Reconnect, and a fresh
   authorize round clears the mark.
6. The manual paste field behaves exactly as it does today.
7. Every new page and state renders with zero `<script>` tags.
8. All gates pass; the new code is covered by the httpmock dance tests;
   suite coverage stays above 92% of lines.

## Open Questions

1. Proxy headers vs an explicit public-URL field for deployed installs —
   the first implementation task validates this behind a reverse proxy;
   the fallback field is in scope only if headers fail.
2. Does adding a token mid-run (config is read at run start) need any
   guard, or does today's manual-save behavior carry over unchanged?
   (Working assumption: carry over unchanged.)
