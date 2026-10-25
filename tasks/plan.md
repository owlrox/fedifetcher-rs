# Implementation Plan: OAuth Connect for Instance Authorization

## Overview

Implement SPEC-oauth-onboarding.md: the config page grows an account-
management section around the existing access_tokens field — connect an
instance account through Mastodon's OAuth authorization-code flow (dynamic
per-server app registration, authorize redirect, callback exchange,
verify_credentials), see connected accounts, disconnect (revoke) one, and
reconnect one whose token died. Zero JavaScript; the manual paste field
stays untouched.

## Architecture Decisions

- **No OAuth crate.** The client is three HTTPS calls plus a redirect;
  hand-rolled on the existing `HttpClient` in a new `src/oauth.rs`, matching
  the crate's one-concern-per-file layout and the no-new-dependencies
  boundary.
- **Origin detection from headers, fallback pending validation.** The
  public origin (for `redirect_uri`) is derived from `Host` +
  `X-Forwarded-Proto`/`X-Forwarded-Host`, computed by one function used
  identically at registration and callback time. If deployed validation
  (Task 2's checkpoint) fails, an explicit public-URL config field is the
  ask-first fallback.
- **CSRF state in a short-lived cookie.** One new cookie
  (`oauth_state`, HttpOnly, SameSite=Lax, 10-minute Max-Age), namespaced
  away from the existing live-updates preference cookie; the callback
  accepts only a `state` that matches.
- **Token single-source.** The token lives only in config's
  `access_tokens`, round-tripping exactly as a pasted one does
  (spec criterion 2). `oauth_accounts` holds metadata only: server, acct,
  avatar, connected_at, needs_reconnect, and the token in full so the run
  loop can match a 401 back to its row without a network ask — the same db
  already holds the same secret in the config table, so the threat model
  does not change.
- **Index-based token removal stays authoritative.** `save_config` removes
  tokens by index today; when it removes one, any `oauth_accounts` row
  holding that token is dropped with it. Disconnect is the friendly path;
  the form field remains the mechanical one. Both end in the same state.
- **401 marking at the per-token call site.** The run loop builds one
  `HomeServer` per token (run.rs); a 401 answer there marks that token's
  row `needs_reconnect`. Detection rides the existing 401 message path in
  `api/mastodon.rs:373` rather than a new error variant.

## Task List

### Phase 1: The connect dance

- [ ] Task 1: `src/oauth.rs` — app registration and the authorize redirect
  - Acceptance: `POST /api/v1/apps` registers "FediFetcher-rs" on the given
    server and the client_id/secret are cached in the db (`oauth_apps`);
    `authorize_url()` produces the `/oauth/authorize` URL with client_id,
    all three scopes, state, and redirect_uri from the shared origin
    function; a cached registration is reused, not re-registered.
  - Verify: `cargo test --test oauth_integration` (registration asserted on
    the mock's request body; URL asserted on its pieces); gates.
  - Files: `src/oauth.rs` (new), `src/db.rs`, `tests/oauth_integration.rs` (new)
  - Scope: Medium

- [ ] Task 2: Callback route, exchange, and denial
  - Acceptance: `GET /oauth/callback` validates `state` against the
    `oauth_state` cookie, exchanges the code at `/oauth/token`, calls
    `verify_credentials`, appends the token to `access_tokens`, records the
    account row, and 303s back to `/config` where the account is listed;
    `error=access_denied` lands on `/config` with a message and adds
    nothing; a state mismatch or a failed exchange adds nothing and says so.
  - Verify: full-dance e2e over httpmock (mock `/oauth/authorize` as a 302
    back to our callback with a code); gates.
  - Files: `src/web.rs`, `src/oauth.rs`, `src/db.rs`, `tests/oauth_integration.rs`
  - Scope: Medium

### Checkpoint: the dance works end-to-end

- [ ] All gates pass; the e2e dance test covers register → redirect →
      callback → exchange → verify → listed account
- [ ] Deployed-shape validation: run the built binary behind a reverse
      proxy (or a forwarding httpmock) and confirm the redirect_uri is the
      PUBLIC origin at both moments — this decides open question 1

### Phase 2: Account management

- [ ] Task 3: The account list, Disconnect, and manual-path consistency
  - Acceptance: the config page lists connected accounts (acct, avatar,
    connected date) above the manual field; Disconnect POSTs, revokes the
    token upstream (`/oauth/revoke`), removes it from `access_tokens`, and
    drops its row; a token removed through the form's `remove_access_token`
    index path also drops its row; the manual paste field and the existing
    tooltips behave exactly as today; no `<script>` anywhere.
  - Verify: web_integration tests (list renders, disconnect 303 + upstream
    revoke received, index-removal cleanup); gates.
  - Files: `src/web.rs`, `src/db.rs`, `tests/web_integration.rs`
  - Scope: Medium

- [ ] Task 4: The reconnect lifecycle
  - Acceptance: a 401 on a connected account's API calls during a run marks
    that row `needs_reconnect` (matched by token, no network ask); the row
    renders a Reconnect button that starts a fresh authorize round for that
    server; completing it clears the mark; an unmarked account's row is
    unchanged.
  - Verify: run-path test (401 from httpmock → row marked) + web test
    (Reconnect renders, completing clears); gates.
  - Files: `src/run.rs`, `src/api/mastodon.rs`, `src/db.rs`, `src/web.rs`,
    `tests/run_integration.rs`, `tests/web_integration.rs`
  - Scope: Medium

### Checkpoint: account management complete

- [ ] Spec success criteria 1–7 each map to a passing test
- [ ] Review with human before Phase 3

### Phase 3: Polish

- [ ] Task 5: README, tooltips, and coverage
  - Acceptance: the README's config section documents the connect flow and
    keeps the manual path as fallback; the connect section carries the
    same tooltip discipline as every other config field;
    `cargo llvm-cov --summary-only` shows suite coverage still above 92%
    of lines with `src/oauth.rs` covered by the dance tests.
  - Verify: gates + coverage run.
  - Files: `README.md`, `src/web.rs`, `docs/ideas/oauth-onboarding.md` (checkboxes)
  - Scope: Small

### Checkpoint: complete

- [ ] All acceptance criteria met; ready for review

## Risks and Mitigations

| Risk | Impact | Mitigation |
|------|--------|------------|
| redirect_uri differs between registration and callback (proxy header asymmetry) | High — the dance fails with an opaque instance error | One origin function used at both moments; Task 2's checkpoint validates behind a proxy before any UI work lands on top |
| `httpmock` cannot express the token exchange's form semantics | Medium — e2e dance untestable | The exchange is asserted on the mock's received form fields; a failing fit downgrades only that assertion to the response side |
| State cookie collides with the preference cookie | Low — CSRF check misfires | Distinct names, distinct Max-Age; covered in Task 2's tests |
| 401 marking fires for causes other than a dead token | Medium — false "needs reconnect" | Only exact 401 answers mark; 403 and network errors keep today's behavior |
| Mid-run config change interacts with connect | Low — spec open question 2 | Carry today's save-during-idle behavior unchanged; connect while a run holds the lock behaves like a manual save |

## Open Questions

- Open question 1 (origin detection) is deliberately scheduled as Task 2's
  checkpoint, before Phase 2 builds on it.
- Open question 2 is resolved by assumption (carry over unchanged),
  recorded here per the spec.
