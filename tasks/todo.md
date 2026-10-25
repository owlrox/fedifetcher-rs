# Tasks: OAuth Connect for Instance Authorization

Plan: tasks/plan.md · Spec: SPEC-oauth-onboarding.md · One-pager:
docs/ideas/oauth-onboarding.md

## Phase 1: The connect dance

- [x] Task 1: `src/oauth.rs` — app registration and the authorize redirect
  - Acceptance: registration cached per server in `oauth_apps`;
    `authorize_url()` carries client_id, three scopes, state, redirect_uri;
    cached registrations are reused
  - Verify: `cargo test --test oauth_integration`; gates
  - Files: src/oauth.rs, src/db.rs, tests/oauth_integration.rs
- [x] Task 2: Callback route, exchange, and denial
  - Acceptance: state checked, code exchanged, account appended and
    listed; denial and state mismatch add nothing and say so
  - Verify: full-dance e2e over httpmock; gates
  - Files: src/web.rs, src/oauth.rs, src/db.rs, tests/oauth_integration.rs

## Checkpoint: the dance works end-to-end
- [x] Gates pass; origin consistency behind a proxy validated with
      forwarded headers in the oauth suite (a real reverse proxy re-check
      remains a deploy-time step)
- [x] Task 2's e2e dance covers register → redirect → callback →
      exchange → verify → token in access_tokens

## Phase 2: Account management

- [x] Task 3: Account list, Disconnect, manual-path consistency
  - Acceptance: list renders acct+avatar; Disconnect revokes, removes,
    drops; index-path removal drops too; manual field unchanged; no
    `<script>`
  - Verify: web_integration tests; gates
  - Files: src/web.rs, src/db.rs, tests/web_integration.rs
- [ ] Task 4: The reconnect lifecycle
  - Acceptance: 401 marks the row; Reconnect renders; fresh authorize
    clears the mark
  - Verify: run-path + web tests; gates
  - Files: src/run.rs, src/api/mastodon.rs, src/db.rs, src/web.rs,
    tests/run_integration.rs, tests/web_integration.rs

## Checkpoint: account management complete
- [ ] Spec success criteria 1–7 each map to a passing test
- [ ] Review with human before Phase 3

## Phase 3: Polish

- [ ] Task 5: README, tooltips, coverage
  - Acceptance: README documents the flow; tooltip discipline held;
    coverage above 92% of lines
  - Verify: gates + `cargo llvm-cov --summary-only`
  - Files: README.md, src/web.rs, docs/ideas/oauth-onboarding.md

## Checkpoint: complete
- [ ] All acceptance criteria met; ready for review
