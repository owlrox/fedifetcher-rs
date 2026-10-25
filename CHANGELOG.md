## 1.3.5

Drop the unused leptos_axum dependency.

It was never referenced: pages are rendered with leptos' own to_html
inside plain axum handlers, and AxumState is axum::extract::State
renamed. Removing it takes 25 crates out of the lockfile.

## 1.3.4

Test connection picks which account or token to test.

The Test connection form gains a picker of every configured identity:
connected accounts by their acct and their own server, pasted tokens by
their masked value. The test asks the picked identity's own server, and
an index that no longer matches is a failure instead of a silently
different test. The user and posts lookups keep using the first token.

## 1.3.3

Keep the remove label beside its checkbox.

## 1.3.2

Pin that the dashboard counts connected accounts in its token total.

## 1.3.1

Trim the token row's hints.

The removed "Tokens are never shown in full" line said what the
tooltip already says, and its extra space goes with it. The remove
checkbox's label now reads left of the checkbox instead of after it.

384 tests pass; clippy -D warnings and fmt clean.

## 1.3.0

Polish the config page's accounts and tokens.

Missing-permissions and rejected-token warnings ride in a red box
(.warn-box, #c44 border and text), and the account rows lay out as a
flex line: avatar and user line left, the Reconnect and Disconnect
buttons right (margin-left:auto on the first action form). The
Reconnect button rides the same warning mechanism both warnings use.

The token rows' remove checkbox sits right of the masked token
(token-row/token-remove classes) instead of below it. The "Hover a
field" hint moves inside the save form — its wording now names what
the tooltips carry (what the option does, its documented default, the
API scopes its task spends) — and a second Save configuration button
sits under it, above the table, so a long form saves from either end.

The connect button says "Connect your Fediverse account". Disconnect's
/oauth/revoke removes FediFetcher-rs from the user's Authorized apps
view (that list is built from live tokens); the inert app registration
left on the instance's developer side is invisible there.

384 tests pass; clippy -D warnings and fmt clean.

## 1.2.27

Cleared fields reset to default, rows warn on missing scopes, richer tooltips.

Three findings from the user's live round.

Clearing a number field and saving errored ("\"\" is not an integer").
For the numbers, booleans, and lists, a cleared field now means the
documented default: apply substitutes the value from Config::default
itself — the one table of defaults, which the README documents and a
test keeps honest — so clearing reply_interval_in_hours saves 0. The
optional strings keep their "empty is unset", the required fields keep
their refusal, and rate_limit_cooldown's empty string means the
default series while an explicit empty list still means no retries.

The connected-accounts rows now say when a connection is short of
permissions: the page compares the config's current scope ask against
the scopes the account's registration was granted under, and a row
whose connection predates a task's scopes reads "The enabled tasks
need more permissions than this connection has — reconnect:" with the
Reconnect button (the dance recomputes the ask, re-registers, and
re-consents). A reconnect also attempts the stale tokens' revoke
upstream — best effort, refusal logged; a scope growth re-registers
the app, so the old app's revocation can be refused, and the log says
so.

The tooltips now carry three lines: what the option does, its
documented default (spelled from the crate's own defaults — "0 (off)",
"true (enabled)", the cooldown series comma-separated), and the API
scopes the option's task spends, with whether they are always granted
or asked only while the task is enabled. The lines are newlines in the
title attribute, which browsers render as line breaks; bolding is not
possible in a title attribute and would take a CSS-tooltip rework.
The inline "(default: …)" fragments the uniform line replaces are
gone, and the access-tokens tooltip's stale scope list is corrected.

384 tests pass; clippy -D warnings and fmt clean.

## 1.2.26

Ask only for the scopes the enabled tasks need.

The user asked whether too many scopes ride along with the connect —
auditing every HomeServer call against its enabling option answered
yes, and found the mirror-image bug too.

The scope ask is now derived from the config at connect time
(oauth::scopes_for): the core three the context work cannot do without
(read:search, read:statuses, and read:accounts for the dance), plus
one scope per enabled task that needs its own — read:lists (from_lists),
read:notifications (from_notifications), read:bookmarks (max_bookmarks),
read:favourites (max_favorites), read:follows (the follow backfills),
admin:read:accounts (reply_interval_in_hours, whose admin API is that
scope's only consumer). A default install asks for three scopes where
it asked for four; the scary admin scope appears only when the task
that uses it is on. The registration cache's scopes column makes a
changed ask self-healing — enable a task, reconnect, and the instance
re-consents on the wider set — and the disconnect path now uses the
cached registration alone (cached_app), since revoking with a
re-registered app's credentials would orphan the token from its app.

The audit also found the under-granting: five task scopes were in
neither the OAuth ask nor the README's manual-token list, so those
tasks 403'd for users who followed the docs. The README's setup and
config-table entries now carry the full option-to-scope map, and the
403 message for the favourites task names read:favourites — the scope
that exists — instead of the invented read:favorites, whose own test
was pinning the misspelling and now asserts the fix.

The connect form's server field carries the required attribute: the
browser refuses an empty submit before anything moves, with the
server-side 400 as the backstop for anything that is not a browser.

382 tests pass; clippy -D warnings and fmt clean.

## 1.2.25

Finish the OAuth onboarding: README, checkboxes, coverage.

Task 5, the plan's last: the README's first-run setup leads with the
connect flow — type the server's name, approve on the instance's own
site, done, with the four scopes and the Disconnect/Reconnect buttons
explained — and keeps the manual token path as the documented fallback
for headless installs and instances that restrict OAuth applications;
the config table's access_tokens entry names both roads. The one
pager's assumptions are checked off with what the validation found
(the scope list needed read:accounts; the origin question resolved for
forwarded headers, real-proxy recheck deploy-time), and the plan's
checkpoints record the live verification rounds.

Coverage: the oauth module's error arms get one test each for the
registration (refused, answered without the secret), the exchange, the
who-ask, and the revoke — src/oauth.rs is now 98.2% of lines, and the
suite is at 92.0% lines / 95.5% functions, above its bar. Along the
way: httpmock serves the first matching mock, so sequential-behavior
tests retire their mocks with up_to_n_times(1) — the lemmy error test
had passed vacuously on the same semantics and its assertions stand
unchanged.

380 tests pass; clippy -D warnings and fmt clean.

## 1.2.24

Log the OAuth lifecycle and what each config save changed.

The user's live round found the OAuth lifecycle silent in the logs
table, and a removal through the config page silent too.

The lifecycle now speaks where the runs do: the dance's start
("Starting the OAuth dance for example.social"), its success
("Connected @alice on example.social (token 6hi7w***Konng)") and the
disconnect ("Disconnected @alice on … (token …)") all land in the logs
table — the token only ever masked, per the spec's boundary. The
revoke-refusal warnings were already in place; a test walks the full
connect-and-disconnect lifecycle through the real logs pipeline
(logging::setup) and asserts both lines, and that no whole token
appears.

The config save's story is now one INFO line naming every changed
option — the changed-options call-out existed but at DEBUG, invisible
at the default level, and the INFO line named nothing. A save changing
the server and adding a token reads: "Configuration saved to the
database (changed: server, access_tokens (added token
saved***67890))". Removals are called out the same way; a reorder with
no add or remove says so; the binary test proves the line for a real
save through the running web UI, including that no whole token
appears.

379 tests pass; clippy -D warnings and fmt clean.

## 1.2.23

Re-register stale-scope apps and log the dance's failures.

Two findings from the third live round.

The instance refused the authorize with "The requested scope is
invalid, unknown, or malformed": the dance asked for four scopes —
1.2.22's read:accounts joined — but the app registered on the instance
back in the 1.2.21 round carried the old three, and an instance
validates the authorize-time scopes against the registered ones. The
cached registration now carries its scope ask (oauth_apps gains a
scopes column, migrated the trigger_posts way on existing databases),
and registered_app re-registers — and updates the row — whenever the
cached ask differs from the current one. A future scope change heals
itself the same way. The re-registration creates a second app entry on
the instance's applications page; the old one is inert and deletable
by hand.

The dance's failures were also invisible on the Logs page: save_error
rendered the message and said nothing to the logs table. It now
emits an ERROR event with the same message, so every failed save and
every failed dance step lands where the run history's failures live.
The binary test proves it end to end: connect to an unreachable
instance, and the logs table records the refused registration.

377 tests pass; clippy -D warnings and fmt clean.

## 1.2.22

Ask for read:accounts: verify_credentials requires it.

The second live round of the dance failed one step further along, and
the status code carried the diagnosis: 401 was no bearer, 403 is a
bearer with an unauthorized scope. The scope list was the README's
fetching list — read:search, read:statuses, admin:read:accounts — but
verify_credentials demands read:accounts, which fetching never calls
and so the list never carried. The dance does; the ask now includes it.

The instance will show the consent screen again on the next connect
(the scope set changed since the earlier approval), which is the
re-confirmation working as designed. The manual token path needs no
change: nothing it does calls verify_credentials.

375 tests pass; clippy -D warnings and fmt clean. Tested by hand.

## 1.2.21

Send the fresh token when asking whose it is.

The first real round of the dance failed where the tests had not:
"checking whose token it is failed with status 401". exchange_code
traded the code for the token correctly and then asked
verify_credentials with a bare client — no Authorization header — and
the instance, correctly, declined to name an account to an unauthenticated
caller. The e2e dance test's verify_credentials mock accepted any
request, so the omission passed every gate.

Prove-it: the mock now demands Authorization: Bearer <the fresh token>,
the dance test failed exactly as the live one did, and the fix sends
the ask through http.authenticated(token) — the same bearer treatment
every home-server call gets, robots.txt included.

375 tests pass; clippy -D warnings and fmt clean. Tested by hand.

## 1.2.20

Add the reconnect lifecycle: a dead token asks to be re-authorized.

Task 4 of the OAuth onboarding plan, TDD: what happens after a connect.

The run loop's per-token site is where a token's death surfaces: a task
failing with the instance's Status code: 401 answer marks that token's
oauth_accounts row needs_reconnect (matched by the token in hand, no
network ask; 403s and network errors keep today's behavior, and the
run still fails as it always has). The config page's row renders a
Reconnect button for a marked account — the same dance, its server
prefilled — and the callback now supersedes: a re-authorization of an
account we already hold removes that account's older tokens from
access_tokens and drops their rows, so the fresh token replaces the
dead one and the mark goes with it. Connecting a brand-new account is
unchanged: nothing to supersede, nothing stale.

The run path needed the same scheme treatment the rest of the codebase
got in 1.2.9: run_once_with_scheme (the listen_with_scheme pattern)
lets the test point a real run at httpmock — the instance refuses the
lists task with 401 and the row ends marked — while run_once stays
https and the binary's behavior is unchanged.

375 tests pass; clippy -D warnings and fmt clean.

## 1.2.19

Add the connected-accounts section and Disconnect.

Task 3 of the OAuth onboarding plan, TDD: the config page becomes
account management.

A Connected accounts section sits above Edit configuration: each row
shows the acct, the avatar, the connected date, and a Disconnect
button; the Connect form (server field with the tooltip discipline,
Connect your Mastodon account) sits beneath; and the dance's return
query string (?oauth=connected|denied|expired|failed|disconnected) is
answered in plain words on the page. The manual paste field and its
tooltips are untouched.

POST /oauth/disconnect finds every row for the account (the same
account may hold more than one token), revokes each token upstream — a
refused revoke is logged and not obeyed, because the token is being
discarded anyway — removes the tokens from the config, and drops the
rows. The mechanical path stays honest too: save_config's index
checkboxes now delete the removed tokens' rows, so the form's
remove_access_token and the Disconnect button cannot disagree about
which accounts are connected.

The oauth_accounts table gains its id column and the row its
connected_at; list, lookup-by-acct, and delete accessors join. The
first cut's swallowed lookup error (unwrap_or_default over a broken
ORDER BY) is what hid the missing column — the config page now warns
when it cannot read the accounts instead of rendering an empty list.
saved_config, the web tests' readback helper, reads leniently: a
config with zero tokens is legitimate after a disconnect.

372 tests pass; clippy -D warnings and fmt clean.

## 1.2.18

Add the OAuth callback: the code is traded and the account connects.

Task 2 of the OAuth onboarding plan, TDD: the dance's web side.

POST /oauth/connect registers the app on the named server (cache-first,
from Task 1), generates a fresh state, and 303s the user to the
instance's own consent screen — setting the ff_oauth_state cookie
(HttpOnly, SameSite=Lax, ten minutes) which remembers both the state
and the server the dance is on with, because the instance's redirect
back carries only the code and the state.

GET /oauth/callback refuses anything that is not the dance we started:
a state mismatch, a missing cookie, or the instance reporting a denied
consent each land back on /config with the reason in the query string
(?oauth=denied|expired|failed) and nothing added. A real callback
trades the code at /oauth/token, asks verify_credentials whose token it
is, appends the token to access_tokens (the same mutex swap
save_config does), records the account row in the new oauth_accounts
table, clears the state cookie, and 303s back with ?oauth=connected.

The web app's AppState now holds the instance client the OAuth handlers
speak through: app_with_scheme builds it (production app() keeps
https), which is also how the tests point the whole dance at httpmock.
The oauth handlers' requests ignore robots.txt, as the home server's
own calls do.

The e2e test walks the dance from the browser's seat: connect →
authorize URL's state carried to the callback → token in access_tokens
→ account row recorded; plus denial, forged state, and — behind
forwarded headers — both ends of the dance naming the public origin,
the plan's origin-validation checkpoint. A real reverse-proxy recheck
remains a deploy-time step. 368 tests pass; clippy -D warnings and fmt
clean.

## 1.2.17

Add the OAuth app registration and the authorize redirect.

Task 1 of the OAuth onboarding plan, TDD: the RED test named
fedifetcher::oauth and did not compile; this makes it pass.

src/oauth.rs speaks the first half of the authorization-code flow:
registered_app registers FediFetcher-rs on an instance with
POST /api/v1/apps (name, callback redirect_uris, the three scopes) and
caches the client credentials in the new oauth_apps table — asked once
per server, never once per visit. authorize_url builds the instance's
own consent-screen URL with every parameter the dance needs: client_id,
redirect_uri, response_type, scope, state. public_origin derives the
public origin the instance must redirect back to from the forwarding
headers a reverse proxy sets, falling back to the request's own Host —
one function, used identically at registration and at the callback, so
the two redirect URIs cannot disagree. The instance scheme follows the
http client's, like every lookup since 1.2.9.

HttpClient gains post_form: urlencoded POST on the same robots and 429
cooldown path as get, by generalizing the private request into a Body
enum (none, JSON, form) rather than duplicating the retry logic.

tests/oauth_integration.rs: httpmock plays the instance; the
registration request's form fields, the registration-once cache
behavior, the authorize URL's pieces, and the forwarding-header origin
are each asserted. 364 tests pass; clippy -D warnings and fmt clean.

## 1.2.16

Plan the OAuth onboarding implementation.

tasks/plan.md and tasks/todo.md break SPEC-oauth-onboarding.md into
five tasks across three phases, each with acceptance criteria,
verification, files, and size. The architecture decisions the plan
fixes: no OAuth crate (three HTTPS calls on the existing HttpClient,
new src/oauth.rs); origin detection from proxy headers by one function
used identically at registration and callback, with the explicit
public-URL field held as the ask-first fallback; CSRF state in one new
short-lived SameSite=Lax cookie; the token single-sourced in
access_tokens with oauth_accounts holding metadata plus the token for
secret-safe 401-to-row matching; index-based token removal staying
authoritative with oauth_accounts rows cleaned up when it fires; and
401 marking riding the existing per-token call site rather than a new
error variant. Risks tabled with mitigations, the origin question
deliberately scheduled as Task 2's checkpoint before Phase 2 builds on
it.

cargo test (360), clippy -D warnings and fmt pass.

## 1.2.15

Write the OAuth onboarding spec.

SPEC-oauth-onboarding.md turns the one-pager into the build contract:
objective (zero secrets copied by hand, config page as account
management), the six Mastodon endpoints used with no new dependencies
(the OAuth client is hand-rolled on the existing HttpClient), a new
src/oauth.rs beside the web routes, two new db tables (per-server app
registrations and per-account metadata with needs_reconnect), the
testing strategy (httpmock plays the instance; the authorize step is
asserted on the redirect's Location), boundaries (schema changes and
new dependencies ask first; a script tag or an unmasked secret never),
and eight testable success criteria including denial, revoke-on-
disconnect, and the 401 reconnect lifecycle. Open questions: proxy-
header origin detection versus an explicit public-URL field, and
whether mid-run config changes need a guard.

cargo test (360), clippy -D warnings and fmt pass.

## 1.2.14

Record the OAuth onboarding one-pager.

The idea-refine session's output, saved as docs/ideas/oauth-onboarding.md:
an account-management section on the existing /config page — dynamic
per-server app registration, the authorize redirect, a callback route,
an account list with disconnect/revoke and a 401-reconnect prompt, and
the manual paste field kept unchanged below. One consent screen carries
all three scopes (decided in the session); scope staging, the
onboarding-first wizard, the pairing-code flow and refresh tokens are
recorded as not-doing with their reasons. The assumptions to validate
before a spec: proxy-header origin detection, the scope list itself,
and open app registration on target servers.

cargo test (360), clippy -D warnings and fmt pass.

## 1.2.13

Sharpen the wafrn note with what their own API serves.

Reading wafrn's backend on Codeberg: the Mastodon-compatible context
route needs the requesting user's id to compute visibility and speaks
obfuscated status ids, so unauthenticated /api/v1 reads are a design
decision, not an oversight. Their own API does read public posts
unauthenticated (GET /api/v2/post/{uuid}, verified live; swagger.json
sits in their repo), but serves the post alone — no replies — so it is
no context source either. The known-incompatible note now says all of
this, so nobody "just uses their own API" without checking.

cargo test (360), clippy -D warnings and fmt pass.

## 1.2.12

Record wafrn as known-incompatible, with the evidence.

A user's run hit app.wafrn.net and logged "runs wafrn and no supported
API is known for it". Investigated rather than guessed: wafrn advertises
a Mastodon-compatible API (its /api/v1/instance says 4.3.0 compatible),
but that layer refuses every unauthenticated request with 401 — public
timelines, account lookup, statuses, and context; only /api/v1/instance
and /api/v1/instance/peers answer. FediFetcher's remote fetches are
deliberately unauthenticated, so adding wafrn to a flavor's software
list would trade today's one cached skip line for a 401 error per URL.
Upstream FediFetcher has no wafrn mention either.

So the software_apis comment's known-incompatible list (where gotosocial
already sits for the same token-gating reason) gains wafrn with the
evidence and the date, answering "should we support wafrn?" for the
next reader. No behavior change: the URL's context is still skipped,
once, with its reason logged.

cargo test (360, all green three times over; one transient failure
during the recompile window did not reproduce), clippy -D warnings and
fmt pass.

## 1.2.11

Canary the binary tests' state-resolution assumptions.

The binary tests pointed the child at its state through XDG_STATE_HOME
and assumed its working directory held no artifacts/ directory. Both
held, but a failure would have surfaced as a missing run row pointing
at nothing — and if XDG_STATE_HOME support ever disappeared, the child
would have fallen through to the real ~/.local/state/fedifetcher and
written there.

Now HOME is pointed into the same sandbox behind XDG_STATE_HOME, and
every test that runs the binary's main body ends with a canary: neither
a legacy artifacts/ directory in the child's working directory nor a
.local tree under its HOME may hold state. If default_state_dir's
precedence ever changes, the test names the broken assumption instead
of failing later on a missing row, and nothing escapes into the
developer's real state. The precedence claim itself was verified
against the real binary: with artifacts/ present the database lands
there, without it under XDG_STATE_HOME, exactly as the tests seed it.

cargo test (360, all green), clippy -D warnings and fmt pass.

## 1.2.10

Test the binary as a process and Lemmy's failures, closing the audit.

The test audit left three gaps deliberately dark; this lights all three.

The Lemmy client's error arms (71.8% lines) were mockable all along —
they just had no tests. Six e2e tests now cover the community listing
path, the unknown profile and post URL shapes, each way a paged fetch
can fail (500, unreadable JSON, entries without posts), a comment's
context resolved through its post (with the post id arriving as a
number and as a string), and the unreadable post and comment list
shapes. Lemmy is now 90.6% of lines with every function executed.

main.rs (0%) and web.rs's serve() startup are covered by a new
binary_integration suite that runs the real binary as a child process
(via CARGO_BIN_EXE) with its own state home through XDG_STATE_HOME: the
version flag; a run with a saved config finishing clean and recording
itself; a run without a config refused critically with exit 1; serve
answering /healthz with no configuration at all; the startup sweep
marking a dead process's run row interrupted; the streaming refusal
warning reaching the logs table; the stream listener spawning when the
config asks (and reconnecting on its growing delays when the server is
unreachable); and a taken port failing with the reason and exit 1.
main.rs is now 85.5% of lines with every function executed; its
remaining dark lines are the db-open failure arms, which would need a
database the process cannot open.

Coverage is 92.2% of lines and 95.7% of functions (was 91.0%/94.3%).
360 tests pass three times over; clippy -D warnings and fmt are clean.

## 1.2.9

Mock server discovery end to end and close the audit's coverage gaps.

The test audit found the suite strong (88.9% lines, no flakes, polling
not sleeping) with one structural blind spot: every server lookup and
URL-shape dispatch hardcoded https, so no test could reach server
discovery, the nodeinfo-to-host-meta fallback, or the context scan's
remote half. The 1.2.6 write-up deferred exactly this.

The fix follows the flavor clients' own with_scheme idiom, moved to the
one object every lookup already receives: HttpClient carries the scheme
(new is https, with_scheme is for tests), and nodeinfo,
server_from_host_meta, client_for and the replied-toot URL builder all
take theirs from http.scheme() — no signature changes anywhere else.
The two regexes that read a server name out of a location or lrdd
template accept http as well as https, since servers may serve either;
the URL parsers stay https-only, as the fixtures' https-spelled URLs
reflect.

New tests, 23 of them: the servers suite grows from 1 to 12 (discovery
success, the host-meta fallback and each of its refusals, the unreadable
announcement shapes); backfill covers the add_user_posts loop end to end
(resolved post's context added, unresolvable post counted as failed and
the account left unmarked, unreachable server, profile URL the flavor
cannot read recorded in its target set); context covers the scan against
a real mock server (fetch, local-URL filter, unresolved count, the
recently-checked skip), the reply chase through its redirect including
the unfindable case, and context_urls; stream covers the two database
bookkeeping failures (state unloadable, run history unwritable). The
flavor e2e tests now run through the Client enum, covering its dispatch
arms.

Test infrastructure: tests/common is split into common (temp dirs,
configs), fixture (databases), runner (run contexts) and capture (log
capture), so each suite compiles only the helpers it uses and the
#![allow(dead_code)] is gone; web_integration's seven copies of the same
poll loop are one page_containing helper. AGENTS.md's testing section
records the scheme convention and how to measure coverage.

Coverage is now 91.0% of lines (was 88.9%): servers.rs 92.6 (was 73.6),
backfill.rs 94.3 (was 74.7), context.rs 94.8 (was 77.5), api/mod.rs
91.1 (was 75.6), stream.rs 89.5 (was 86.9). 347 tests pass three times
over; clippy -D warnings and fmt are clean.

## 1.2.8

Add CHANGELOG.md generated from the commit history.

The version write-ups in the commit messages were already release-note
quality, so the changelog is generated from them rather than maintained
separately. ./changelog.sh (plain git + awk, no dependencies) walks the
history and emits one section per version, newest first: the subject as
the lead line and the write-up body verbatim, minus the Generated
by/Co-Authored-By trailer.

A commit's version is the (x.y.z) suffix of its subject; commits written
before that convention (everything below 1.0.5) fall back to the version
in the Cargo.toml committed with them — which also recovers 1.0.25,
whose subject forgot the suffix — and the two commits that still carried
the upstream 8.2.3 version are grouped under "Early development
(pre-1.0.3)". Shared versions group under one heading.

AGENTS.md's versioning section now requires every version commit to
regenerate CHANGELOG.md in the same commit, and the README links it.

Verified: the script is idempotent across reruns, no empty headings, no
commit trailers leak into the output, and all 68 commits land under a
versioned heading. cargo test (all 19 binaries), clippy and fmt pass.

## 1.2.7

Tighten the build with lint tables and release overflow checks.

The crate had no [lints] or [profile] sections, so it ran on the
compiler and clippy defaults. Added, in Cargo.toml:

- [lints.clippy]: dbg_macro, todo and unimplemented are denied, so a
  leftover debug macro or placeholder fails the build instead of
  shipping (verified: a dbg! probe is rejected). cast_lossless and
  unchecked_time_subtraction are warned, catching the silent-loss
  shapes: an `as` cast that could truncate if the types change, a
  wall-clock subtraction that could underflow. The one existing
  cast_lossless fire is fixed: config.rs's bool-to-i64 coercion now
  uses i64::from instead of `as`.
- [profile.release]: overflow-checks = true, so the deployed binary
  panics on arithmetic overflow instead of wrapping silently; the cost
  is a few percent on a background job.

Wider lints were measured and left off deliberately, with their price
today recorded in a comment in Cargo.toml: missing_docs (~190
undocumented pub fields, variants and methods), the possible_truncation/
wrap/sign_loss cast group (~28 audited sites), indexing_slicing (25
sites, including deliberate byte slicing in robots and http),
print_stderr (logging.rs's two eprintln fallbacks, used exactly when
the database log write itself has failed) and unwrap_used/expect_used
(mutex locks and compiled-in regex patterns, plus unit tests).

unsafe_code stays at the default warn: store.rs's one unsafe block
(libc::kill signal 0, the lock liveness probe) has no safe std
equivalent. cargo test (all 19 binaries), clippy (0 warnings) and fmt
all pass.

## 1.2.6

Close test coverage gaps in tasks, logging, context and backfill.

Measured with cargo-llvm-cov, the suite covered 84.5% of lines; the
worst gaps were the task runner (35%), the log pipeline (38%), the
context scan (60%) and server discovery (69%). Added integration and
unit tests for the behaviors those numbers said were untested:

- tests/tasks_integration.rs: run_enabled_tasks runs each task only
  when its setting asks, a canceled run stops at the checkpoint after
  the first task, reply_toots keeps only recent unseen replies and
  survives one user's statuses failing, and a timeline post's author,
  mentions and boosted account are backfilled
- tests/logging_integration.rs: an unknown log level is refused, lines
  reach the logs table with the config's level spellings and shutdown
  has written them all out, and a stored row renders in the configured
  format
- tests/context_integration.rs: add_context_urls counts added, failed
  and already-seen URLs, and a replied-toot lookup is answered from the
  cache
- tests/servers_integration.rs: an unreachable server is remembered as
  a failed lookup with its reason
- tests/api_integration.rs: a post on a server that speaks none of our
  APIs is not looked up, and the answer is cached
- tests/backfill_integration.rs: an unparseable profile URL, a server
  with no known API and an unreadable profile shape are each handled,
  and a post without possible context is resolved and remembered
- src/servers.rs: host-meta parsing errors (no lrdd link, no template,
  no server in the template, not XML) and each flavor's state key and
  name

Line coverage is now 88.9%; tasks.rs 95%, logging.rs 94%, mastodon.rs
88%. Remaining gaps are nodeinfo's host-meta fallback paths and the
api::Client dispatchers, which hardcode https and cannot be pointed at
a local mock server. cargo test, clippy and fmt all pass.

## 1.2.5

Support Arch in deploy script and document distro Rust versions.

deploy.sh now detects pacman and installs git, base-devel, cmake, perl,
and curl on Arch (systemd service and useradd user creation, like Debian
and Fedora). The README's Deployment section names the supported
package managers and adds a table of distro Rust versions: Alpine Edge,
Arch, and Fedora Rawhide ship Rust 1.99.1+ and are used directly, while
openSUSE Tumbleweed (1.99.0), Debian 13 (1.85.1), and Debian 14 (1.97.1)
fall back to rustup, as do stable Alpine and Fedora releases. Homebrew
on macOS is noted as new enough for building from source.

## 1.2.4

Add deploy script and document deployment.

deploy.sh installs FediFetcher on a fresh Alpine, Debian, or Fedora
LXC/VM: it detects the package manager, installs the build dependencies
(git, C/C++ toolchain, cmake), fetches the source, installs Rust via
rustup when the system toolchain is older than the rust-version in
Cargo.toml, builds with cargo build --release --locked, installs the
binary to /usr/local/bin, creates a fedifetcher system user and state
directory, and sets up a serve service (systemd, or OpenRC on Alpine
without systemd). Re-running upgrades in place and is idempotent: a
no-change run restarts nothing.

The README gains a Deployment section with the script's usage and the
manual git fetch / build / reinstall / restart steps, and the
Requirements section now lists the C++ compiler and cmake the build
actually needs.

## 1.2.3

Show running elapsed time in the message column.

A run still in progress has no finished time, so the run history left
the Finished cell blank-worthy but rendered "running for ..." there.
Move that elapsed-time text into Message, where a running row's own
words belong, and leave Finished blank until the run ends.

Verified with cargo fmt, cargo clippy --all-targets -- -D warnings,
and cargo test.

## 1.2.2

Rename test domain fixture.

Replace the fictional test host in the HTTP error-chain tests with an
RFC 2606 reserved name. No behavior changes.

Verified with cargo fmt, cargo clippy --all-targets -- -D warnings,
and cargo test.

## 1.2.1

Recycle stream connections before proxy lifetime.

Some instances sit behind proxies whose edge closes long-lived
HTTP/1.1 SSE connections around 100 seconds; FediFetcher saw those as
repeated "Connection reset by peer (os error 54)" while reading the
stream body. The listener had a 300s silence timeout but no maximum
connection lifetime, so the proxy always reset it first.

Add MAX_STREAM_LIFETIME of 90 seconds: read_stream now returns Closed
when a connection reaches that age, and the existing reconnect loop
treats it like a normal stream end with backoff reset after a stable
connection.

Verified with cargo fmt, cargo clippy --all-targets -- -D warnings,
and two consecutive cargo test runs.

## 1.2.0

Move to Rust 1.99.1 and edition 2024.

Set edition = "2024" and rust-version = "1.99.1" on stable 1.99.1.
Collapse nested if/if-let pairs into edition-2024 let chains across
mastodon, backfill, config, context, http, logging, servers, stream,
timeutil, translate, and web. Apply current rustfmt style to src and
tests, and update the README toolchain requirement.

Verified with cargo fmt --check, cargo clippy --all-targets -- -D
warnings, and cargo test (three consecutive full-suite runs).

## 1.1.15

Record every fetch in the run history.

The web UI's status pill reads the newest runs row, but only the web
Run button and manual fetches wrote one: a run from the command line
(cron) and a stream-triggered fetch held the lock with no row, so the
pill read Idle while FediFetcher was fetching in another process — and
a cron run was missing from the run history entirely.

run_once now records itself, whichever process called it, and the web
handler's own recording moved into it so there is one code path; the
outcome mapping is unchanged (ok, stopped, error). A stream-triggered
fetch records a "Stream: home timeline" row, like a manual fetch's
"Manual fetch: ..." label. The pill now reads Running whenever
FediFetcher is fetching, and cron runs appear in the history.

The pause and stop buttons still only signal this process's runs —
that was already true of manual fetches, and the pill now says so
plainly instead of hiding the activity. AGENTS.md's Web UI behavior
section records the rule. Tests: run history rows for a finished run,
a lock-refused run, a stopped run, and a streamed update. cargo test
(302 passed), cargo clippy --all-targets, and cargo fmt --check all
pass.

## 1.1.14

Read the instance's WebSocket streaming URL over HTTP.

The instance document's configuration.urls.streaming names the
streaming host as a WebSocket URL — it is what Mastodon's WebSocket
connections use — and the SSE listener used it verbatim as the base of
its HTTP GET. reqwest allows only http and https, so the request failed
with "URL scheme is not allowed" and the listener reconnect-looped
without ever connecting.

The scheme is now mapped: wss to https, ws to http, an http(s) URL
taken as it is, and the API's own host as the fallback when the
document says nothing or names nothing recognizable. The streaming
host is still honored; only the scheme is translated. The README's
roadmap item says so now too.

Tests: unit tests for the mapping (wss, ws, http, https, unrecognizable,
no host) and an integration test where the instance document names a
ws:// URL and the listener connects over HTTP and records the event's
post as a trigger post. cargo test (302 passed), cargo clippy
--all-targets, and cargo fmt --check all pass.

## 1.1.13

Queue stream events that arrive during a run.

An update event that arrived while a run held the lock was skipped,
relying on the run's home timeline scan to find the post — and missing
it when home_timeline_length was off or the run had already passed its
home timeline task. Now the post waits in a bounded queue (the latest
ten, deduplicated by URL), and a drainer task spawned with the listener
handles the queued posts in one session as soon as the lock comes free.

The read loop never blocks on the lock: it queues and returns, so the
stream keeps flowing during a run. The drainer polls every second, but
checks the lock file's existence rather than attempting the lock, and
attempts it only when the file is gone — or every sixty polls, because
the attempt is what reclaims a lock a dead process left behind, and
every failed attempt logs. The lock still serializes all state
write-backs, and the drainer stops with the listener's cancel token.

The settings, database, HTTP client, home server, and queue that
handling needs are bundled in a Handler struct rather than adding an
eighth argument to read_stream. Tests: the queue's bounds, dedup, and
take-all; the drainer's cancel; and an integration test that holds the
lock, streams an update, and asserts it is recorded as a trigger post
once the lock is released. cargo test (298 passed), cargo clippy
--all-targets, and cargo fmt --check all pass.

## 1.1.12

Ignore macOS Finder metadata files.

Add .DS_Store to .gitignore so Finder metadata no longer shows up as
untracked noise in git status. Version bumped 1.1.11 to 1.1.12 per
the every-change-gets-a-bump rule; the software itself is unchanged.
cargo test (293 passed), cargo clippy --all-targets, and cargo fmt
--check all pass.

## 1.1.11

Track the README in version control.

The README has existed only as a working-tree file since the root
commit: no commit ever contained it, it was never staged (its blob
was never in the object database), and it is not gitignored — it was
simply never added. Track it now, carrying the 1.1.10 streaming
documentation: the feature bullet, the 'streaming' row in the
configuration table, the checked roadmap item, and the scheduling
note.

Version bumped 1.1.10 to 1.1.11 per the every-change-gets-a-bump
rule; the software itself is unchanged. cargo test (293 passed),
cargo clippy --all-targets, and cargo fmt --check all pass.

## 1.1.10

Stream the home timeline for immediate context fetching.

The roadmap's "Stream instead of poll" item, done as SSE in serve mode.

With the new `streaming` option on (default off), `serve` opens a
long-lived connection to /api/v1/streaming/user with the first access
token, and every `update` event is handled as it arrives: the post is
recorded as a trigger post under the "stream" source and its context is
pulled, exactly as the home timeline scan treats a post — no waiting for
the next run.

The streaming host comes from configuration.urls.streaming in
GET /api/v2/instance, falling back to the API's own host. The stream GET
does not follow redirects, so a cross-host redirect cannot silently lose
the access token. A stream that ends or fails is reconnected after a
growing delay (1s to 5min, reset after a minute of uptime); a refused
token (401/403) stops the listener, since no retry would change it. A
silent connection is re-established after 5 minutes.

Each event is handled under the lock file, like a run, so it never
overlaps one; when a run holds the lock the event is skipped, because
that run is scanning and will find the post. The option reads the config
as it was at startup and applies on restart.

New module src/stream.rs: an incremental SSE parser, the listener loop,
and the per-event handler. reqwest gains its `stream` feature for reading
the event stream. The config page has the option, with a tooltip.

## 1.1.9

Make the Logs page's All pill uncheck every level.

The All pill was a one-way reset: it linked to the unfiltered view, so
clicking it while every level was already shown just reloaded the
page. It is now a toggle like the level pills — every level shown
links to the empty selection (?levels=), where the page shows no lines
and says why, and anything less links back to every level. Its tooltip
follows the action ("Hide every level" / "Show every level"), and the
intro text describes the pills as toggles.

## 1.1.8

Filter the Logs page by level.

The /logs page gains a row of clickable level pills — All, DEBUG, INFO,
WARNING, ERROR — above the log table. Every level is shown by default;
clicking a pill flips that level in the query string (?levels=...),
which reloads the page at once, so the filter is a plain link like the
sort headings and needs no JavaScript. A shown level renders in its
severity color, a hidden one as an outline of it, and the All pill
resets to every level. The column-heading sort links now carry the
active filter, the empty view says "No log entries at the selected
levels." when a filter hides everything, and the live meta-refresh
keeps the filter, as it re-requests the same URL.

db::recent_logs takes the levels to show (None for every level,
Some(&[]) for none) and applies the filter in SQL before the line
limit, so the newest 200 matching lines are shown. The log table's
sortable column headings are unchanged.

## 1.1.7

Interrupted runs report how long they had been going.

When the web UI starts and finds runs still marked running, they
belong to a process that is gone. The interruption message now says
how long the run had been going, from its start to now — the same
elapsed time a stopped run logs. interrupt_stale_runs takes the
current time as a parameter and builds the message per row, falling
back to the plain message when a row's start time does not parse.

## 1.1.6

Read Iceshrimp's /notes/ post URLs.

Iceshrimp speaks the Mastodon API but hands posts out as
Misskey-style /notes/{id} URLs, where the id is the status id the
API addresses the post by (checked against snug.moe: a status's
url is https://snug.moe/notes/asloypdjeaoyizzm and
/api/v1/statuses/asloypdjeaoyizzm answers with that id). The
Mastodon client's POST_PATHS did not know the shape, so parsing
such a URL failed with "the URL is not a mastodon post URL".
Add /notes/{id} to POST_PATHS, with a unit test for the shape and
an updated integration test.

## 1.1.4

State the versioning scheme plainly and record the web UI behavior since 1.1.0.

The Versioning section narrated the scheme's history instead of the
scheme: which commits first named their version, and the one commit
that broke the convention and carries a git note. Replace it with the
rule itself: bump the patch number for bug fixes and small features,
bump the minor number for major or substantial changes or when
specifically requested, never reuse a version. The commit conventions
stay: bump and write-up in the same commit, version in parentheses at
the end of the subject.

Also catch the file up with what changed since it was last touched at
1.1.0: a "Web UI behavior" section recording that run status is
single-sourced from the runs table (the pause flag is the one in-memory
signal), that the dashboard meta-refreshes while a run is in progress
even with live updates off, and that manual fetches and stopped runs'
elapsed times are recorded in run history; a "Testing" section noting
the web integration tests poll instead of sleeping fixed times; and a
note that /about restates the GPL license and warranty disclaimer.

Docs only; patch bump per the scheme.

## 1.1.3

Derive run status from the run history; no /hosts references left.

The dashboard's status used to come from an in-memory UiState read at
render time, while the run history came from the database read a moment
earlier — so a run finishing in between briefly showed an "Idle" pill
next to a history row still marked "running" (and a manual fetch or a
run in another process showed "Idle" for its whole duration). The run
history is now the one source of run status, everywhere:

- `AppState.ui` / `UiState` / `RunStatus` are gone. The run handler no
  longer maintains an in-memory status: `db::start_run` writes the
  "running" row before the task is even spawned, and `db::finish_run`
  completes it, so the row is the status at every instant.
- New `run_in_progress(&state)` helper reads the history's newest row
  (outcome "running") and returns its started timestamp. Every page
  handler passes it to `Page`, whose top-bar `RunControls` pill derives
  from it; the dashboard derives its badge and "Running since" label
  from the same history read it already does, so badge, pill, and
  history rows can never disagree.
- The stop and pause handlers decide their log level ("a run is
  affected" vs "nothing to stop/pause") from the same helper instead of
  the removed in-memory status.
- The pause flag stays in memory: it is the signal the running task
  waits on, not a fact about the past, so there is nothing to derive.
  "Paused" therefore means "a run is in progress AND the flag is set".
- A side effect worth knowing: the pill now shows "Running" during a
  manual fetch and during a run started by another process (the CLI) —
  both were previously invisible. Stop still cannot stop another
  process's run or a manual fetch (each has its own cancel token); the
  pill shows the truth, the button stops this process's run only.

Also audited for leftover /hosts references: the only one is the
deliberate 308 redirect route (plus its doc comment and its tests);
no link, form, or page copy points at the old path. The `tab=hosts`
query value is the data page's seen-hosts sub-tab, not the old page.

272 tests pass (1 new: the_run_status_comes_from_the_run_history — the
pill and the Run section follow a seeded running row on every page and
return to idle when the row finishes); clippy and fmt clean.

## 1.1.2

Web UI: about page, top-bar run controls, referer redirects, richer run history.

Six changes to the web UI and the run history:

- The top bar's version number is a link to a new /about page: what
  FediFetcher-rs is, and the license it is under (GPL v3-or-later, with
  the upstream MIT notice kept in NOTICE.md). There is no alert box to
  open — the UI is JavaScript-free by design — so the "about" is a page
  of its own. The top bar is only as wide as the page below it (both
  64rem, same padding).

- The top bar now carries the run controls on every page: the status
  pill (Idle / Running / Paused) and short Start / Pause / Resume / Stop
  buttons with explanatory tooltips. The dashboard keeps its long-label
  buttons and its own status line.

- Pressing start, stop, pause or resume redirects back to the page the
  button was pressed on (the Referer, falling back to the dashboard), so
  the page load that answers the POST shows what the button did. The
  dashboard also refreshes itself while a run is in progress whatever
  the live-updates toggle says — a paused dashboard would otherwise
  never show the run's elapsed time ticking or its row flipping to
  finished.

- A manually stopped run logs how long it ran: "Stopped after 0:00:03.2"
  instead of "Stopped."

- The dashboard's State section now counts the database's tables too —
  trigger posts, private posts, opted-out accounts, known context posts,
  the robots.txt cache, log lines, and runs recorded — alongside the six
  counts the state itself remembers.

- A manual fetch from the data page is recorded in the run history like
  a run: the row starts the moment the fetch starts, and ends with
  outcome and message "Manual fetch: {kind} {key}", so the history tells
  it apart from a scheduled run's "Processing finished in ...".

271 tests pass (5 new: about page, top-bar controls on every page,
referer redirects, manual-fetch run rows, dashboard table counts);
clippy and fmt clean.

## 1.1.1

Add pause and resume for a running fetch.

A run can now be paused where it is and continued later, alongside the
existing stop. A new Pause signal sits next to CancelToken in run.rs:
clones share the flag, and Context::check_in() holds while it is set —
at the same checkpoints a stop lands, between tasks and between the
items of the long loops — then reports whether the run was stopped. A
stop still lands while paused. run_once takes the pause alongside the
cancel token; the CLI passes a fresh one, and a manual fetch keeps its
own, so the run's buttons never touch it.

The web UI's dashboard gains a Pause button next to Stop while a run is
going (Resume takes its place while paused, Stop stays), a "Paused"
status badge, and POST /pause and /resume routes. Starting a run clears
a leftover pause, as it already cleared a leftover stop.

The Python-style fragment functions — reply_toots, replied_toot_server_ids,
context_urls, add_context_urls, and their helpers — now take the run
Context like the rest of the fetch code, instead of pieces of it, so
every loop checks in through ctx.check_in().

Five new tests: the Pause flag and check_in's hold/resume/stop at unit
level, a paused run holding past the point it would finish and a stop
landing while paused at run level, and the pause button pausing a
running run and resume continuing it through the web UI.

## 1.1.0

Bump to 1.1.0: this release carries the GPLv3 relicensing and the web UI rework.

A minor bump, not a patch bump: since 1.0.0 the port has grown the whole
Rustify campaign (one Error enum, no clippy allows, latest dependencies,
US-English sweep, encapsulated State, RFC 3339 dates), the web UI rework
(top-bar live toggle, force-backfill button, the /data route, named API
scopes, the page-load logging toggle), and now the GPL-3.0-only
relicensing with the upstream MIT attribution kept in NOTICE.md. The
version surfaces everywhere it is shown — the web UI header, --version
(just verified: "fedifetcher 1.1.0"), and the startup log line — because
they all read env!("CARGO_PKG_VERSION"); only Cargo.toml changes.

Also fixes the one version-sync break in the history the least-hard way:
4f0b94f bumped 1.0.24 to 1.0.25 without naming the version in its
subject, and now carries a git note recording the version
(`git notes show 4f0b94f`) — no history rewrite. AGENTS.md's Versioning
section now allows minor bumps for bigger steps and records the
one-off, so the next change neither "fixes" 1.1.0 back to a patch bump
nor rewrites the past.

261 tests pass; clippy and rustfmt are clean.

## 1.0.37

Relicense the project GPL-3.0-only.

The port was carrying the upstream's MIT license in its one and only
place: the license field in Cargo.toml. There was never a LICENSE file
in this repo (nothing tracked, nothing in history, no per-file license
headers, and Cargo.lock records no licenses), so the field is the whole
of the in-repo change: license = "GPL-3.0-only", with the full GPLv3
text now in LICENSE.

The code is a derivative of the MIT-licensed Python FediFetcher
(github.com/nanos/FediFetcher, copyright Michael Thomas), and the MIT
license requires its copyright and permission notice to travel with
copies and substantial portions — so NOTICE.md keeps the upstream
attribution and the MIT notice verbatim. GPLv3 is compatible with MIT,
so the port as a whole is now GPLv3-only while the upstream notice is
preserved. AGENTS.md gains a License section so the choice and the
attribution rule are written down where the next change will see them.

261 tests pass; clippy and rustfmt are clean.

## 1.0.36

Gate the web UI's page-load logging behind a config option.

The default log level is DEBUG, so every page load of the web UI was
logged — one line per GET, on stderr and in the log database the Logs
page reads. That is noise until you are watching the UI's traffic, so
the six page-load lines (/, /data, /logs, /live, /config, /debug) now go
through a page_load helper that checks the new log_page_loads option:
off by default, a yes/no field on the config page turns it back on.

Also documents the project's conventions in AGENTS.md: follow the Rust
style guide, no #[allow] (fix the cause — pass the run Context or an
options struct, never a pile of boolean flags), errors go through the
one Error enum with ? everywhere, user-facing text is US English
(Mastodon's own API names keep theirs), the web UI is JavaScript-free
by design, the commit subject ends with the version in parentheses, and
cargo test / clippy / fmt --check must pass before committing.

261 tests pass (new: page loads are logged only when the toggle is on,
plus the config round-trip and tooltip coverage for the option);
clippy and rustfmt are clean.

## 1.0.35

Web UI: top-bar live toggle, force-backfill button, /data route, named API scopes.

Four web UI changes, plus a refactor the new code prompted:

- The live page-refresh toggle moves from the Logs page to the right of
  the top tab bar, on every page, and the top bar is only as wide as the
  page below it. The toggle is a plain link to /live?on=0|1: it sets an
  ff_live cookie and 303-redirects back to the page it was clicked on
  (the Referer's path and query, falling back to /). No JavaScript.
  Logs refreshes while live; the dashboard refreshes while live and a
  run is in progress (previously unconditional when running).

- The data page's opted_out_accounts table gets a per-row "Force
  backfill" button that fetches the account's posts even though it
  opted out (nobot / not indexable). The form posts force=1 to /fetch;
  fetch::force_user_posts and backfill::user_posts take the flag and
  skip the opt-out check when it is set, logging that the opt-out is
  being overridden. Runs never pass it.

- The data page's route is /data, not /hosts (the page was renamed long
  ago; the route was not). Old /hosts links get a permanent 308
  redirect that keeps the query string.

- Every Mastodon API endpoint now names the access-token scope a 401/403
  answer is reported with (read:bookmarks, read:favorites,
  read:follows, read:lists, read:notifications, ...), so a missing
  permission says which one is missing in the logs, the run's failure
  message, and the Debug page.

Refactor: adding the force flag pushed backfill::user_posts past
clippy's argument limit, so it now takes the run Context like its
callers do instead of four separate environment parameters, and the
two force booleans on add_user_posts became a Force struct with named
fields — runs pass Force::default(), the web UI's fetch passes what its
button says. No #[allow] anywhere in the crate.

260 tests pass (5 new: the cookie toggle, a paused dashboard with a run
in progress, the /hosts redirect, the missing-scope 403s, and the
forced fetch of an opted-out account at both the fetch and backfill
layers); clippy and rustfmt are clean.

## 1.0.34

One error enum for the whole crate.

Error was a newtype around a String, with ConfigError and LockedError
beside it and From<String>/From<&str> impls doing the converting. It is
now one thiserror enum in error.rs: what failed is a variant, and the
Display text is still the message that gets logged.

- Db(#[from] sqlx::Error) keeps "Database error: ..."; the 51
  .map_err(db_error)? sites in db.rs collapse to plain ?, and
  retry_locked converts with ex.into().
- DbOpen { path, source } for a database file that will not open, and
  Io { context, source } for file read/write failures, both keeping the
  cause as #[source].
- Http { url, source } and HttpRead { url, source } keep reqwest's error
  chain spelled out in the message; error_chain moved to error.rs with
  its test.
- Json(#[from] serde_json::Error) and Xml(#[from] roxmltree::Error)
  convert at the two parse sites that needed them.
- Config(String), Locked(String) and Fetch(String) replace the
  ConfigError/LockedError newtypes and every bare Error("..."); callers
  that logged the message are unchanged, and tests can still match on
  the variant.
- The three Result<T, String> helpers (nodeinfo, the config coercers,
  look_up_post) return the crate Result; ConfigError and LockedError are
  gone, and so are the From<String>/From<&str> impls and the tuple
  struct's public field (store.rs tests assert on to_string() now).

Every logged message is byte-identical to before. 255 tests pass
(153 lib + 102 integration), clippy is clean, fmt is clean.

## 1.0.33

Wait out and retry the database's write lock.

A run failed to record its trigger posts with "database is locked"
(code 517, then 5) while the web UI and the log writer wrote to the
same database. SQLite lets one connection write at a time, and two
situations fail instead of queueing:

- SQLITE_BUSY_SNAPSHOT (517): record_trigger_posts read first_seen
  inside its transaction, then wrote. In WAL mode a read opens a
  snapshot, and if any other connection commits in between the
  snapshot can no longer be upgraded to a write — the busy handler is
  deliberately not invoked. The web UI's run shares one process and
  pool with the log writer, which inserts a row per log line, so the
  race was constant.
- SQLITE_BUSY (5): a write that arrives while a long transaction — a
  state save, up to 100k rows — holds the write lock gave up after the
  5s busy timeout.

Fixes, layered:

- record_trigger_posts now counts new posts before its transaction and
  keeps the transaction write-only, so no snapshot upgrade can fail.
  (A repeated URI in one scan still counts as new once.)
- Both long write transactions — record_trigger_posts and save_state —
  go through retry_locked: a write that finds the lock held is retried
  a few times with a short pause, which covers a lock held longer than
  the busy timeout and any residual snapshot conflict. is_locked reads
  the extended result code (5 and 517) off the sqlx error.
- The busy timeout rises from 5s to 30s, so short writes (log lines,
  config saves) wait out a long transaction instead of failing.

The schema setup in open() is extracted into create_schema so tests can
build pools with their own busy timeout. New tests: both lock codes are
recognized as locked (and a constraint error is not), and a write
succeeds while another connection holds the lock — the reported
failure, reproduced.

Verified: cargo test 253 passed (151 lib + 102 integration), clippy
--all-targets zero warnings, fmt clean.

## 1.0.32

Finish the US-English sweep: flavour to flavor.

The earlier spelling pass caught favorite/canceled/honor and friends but
missed the other UK word in the codebase: the server-API kind is now
ApiFlavor (was ApiFlavour), with the flavour variables, the FLAVOURS
test const, comments, and log strings following, and the integration
test file renamed flavour_http_e2e.rs to flavor_http_e2e.rs. No
user-facing strings or Mastodon API field names are affected; the only
public surface change is the ApiFlavor rename in fedifetcher::servers.

Verified: cargo test 250 passed, clippy --all-targets zero warnings,
fmt clean.

## 1.0.31

Encapsulate State behind accessor pairs.

The eight collections on State were public fields, so any code holding a
&mut State could restructure them freely. They are now private, with a
getter/mutator pair per collection (seen_urls/seen_urls_mut, ...), the
idiomatic Rust shape for a struct whose fields are implementation detail.
No interior mutability was added: State is already threaded as &mut
State, so plain &self/&mut self accessors keep the borrow checker doing
the work.

Ripples from the encapsulation:

- find_post no longer takes two &mut field borrows (which the accessors
  cannot express in one call); it takes &mut State, matching its callers.
- db::load_state builds State::default() and fills the collections
  through the mutators instead of a struct literal with ..Default.
- &mut state.seen_hosts at the server_info call sites becomes
  state.seen_hosts_mut(); shared borrows passed to save_timestamped_set
  and filter_known_users drop the & and call the getter.
- UserSet::get returns the mutator, so target.get(ctx.state).add(...)
  reads the same as before.

Verified: cargo test 250 passed (148 lib + 102 integration), clippy
--all-targets zero warnings, fmt clean, zero #[allow].

## 1.0.30

Parse RFC 3339 before the legacy shape; share the fraction and offset formatting.

The legacy Python str(datetime) parsing stays: the program itself writes
that shape (fmt_state_ts) and state files from the Python original use
it, so dropping it would need a data migration. Two tidy-ups that change
no behavior:

- timeutil::parse tries the standard RFC 3339 shape first, so the common
  case (timestamps straight from the APIs) parses without building a
  rewritten string; the separator normalization and the Python-shape
  formats remain as the fallback for state-file timestamps.
- the .123456 fraction logic shared by fmt_state_ts, fmt_lock_ts and
  fmt_timedelta becomes one subsecond_fraction helper, and the naive
  local-datetime formatting behind fmt_state_ts/fmt_lock_ts is shared,
  leaving the offset suffix as the only difference between them.

All 250 tests pass, clippy reports zero warnings, cargo fmt is clean.

## 1.0.29

Drop the config-key migration; a spelling change needs no data migration.

The max_favorites rename stays; only the one-time UPDATE that renamed a
stored max_favourites row on open goes away. A database written by an
older version simply reads as the default until the option is saved
again from the config page.

## 1.0.28

Spell the user-facing language in US English.

Every string a user sees, and every name spelled into the database, now
uses US spelling; the Mastodon API's own names are untouched.

- the config option max_favourites is now max_favorites: the field, the
  key stored in the database, the web UI's form field, its hover help
  text, and the task label shown for it. A migration in db::open renames
  the stored key, so an existing database keeps the setting (tested).
- the web UI's posts table header reads "Favorites"; the Mastodon API
  fields it reads (favourites_count, /api/v1/favourites) keep the
  server's spelling — they are the API's names, not ours.
- log lines, docs and comments: favorites, canceled, honors, recognize,
  normalize; CancelToken::is_canceled and the canceled flag, the
  fetch_favorite_context task, HomeServer::favorites, and test names
  follow the same spelling.
- "cancellation" stays: it is spelled the same in both varieties.

The local config.toml (gitignored, holds a token) is no longer read by
the program, so its old key is left alone. All 251 tests pass, clippy
reports zero warnings, cargo fmt is clean.

## 1.0.27

Update every dependency to its latest stable version.

Major bumps, with the small refactors their breaking changes needed:

- reqwest 0.12 -> 0.13: the rustls-tls feature is renamed rustls
  (aws-lc-rs plus the platform certificate verifier)
- sqlx 0.8 -> 0.9: sqlx::query now takes only audited SQL — literal
  strings pass as before, and the seven dynamic statements in db.rs
  (identifier interpolation already guarded by quote_ident or hardcoded
  column lists) are wrapped in AssertSqlSafe, sqlx's explicit opt-in
- axum 0.7 -> 0.8, leptos 0.7 -> 0.8, leptos_axum 0.7 -> 0.8: no code
  changes needed — the router's routes are all static paths, so the 0.8
  path-parameter syntax change does not apply
- roxmltree 0.20 -> 0.21, httpmock 0.7 -> 0.9 (dev): httpmock made the
  request's query_params/headers accessors methods, deprecated hits()
  in favour of calls(), and deprecated When::matches in favour of
  is_true(); the tests follow
- cargo update brings the rest to their latest semver-compatible
  versions (tokio 1.54, clap 4.6, time 0.3.55, thiserror 2.0.21, ...)

leptos stays on the 0.8 stable line rather than the 0.9.0-rc prerelease.
All 250 tests pass, clippy reports zero warnings, cargo fmt is clean.

## 1.0.26

Remove every clippy allow by passing the run context as one parameter.

The five remaining #[allow] attributes are gone, with no behavior change:

- run::Context (moved from tasks.rs, where it was private to the task
  loop) is now the single parameter the long fetch functions take:
  known_context_urls(ctx, toots, source, force),
  add_user_posts(ctx, users, target, force),
  fetch_timeline_context(ctx, posts, source),
  force_user_posts(ctx, acct, target), and add_post_with_context(ctx, post)
  and force_post_context(ctx, uri) follow the same shape. The four
  clippy::too_many_arguments allows disappear with the parameter lists.
- The web UI's manual fetch builds one Context and uses it for all four
  fetch kinds; tests build one via a new common::run_context helper.
- The clippy::type_complexity allow on a db test becomes a TriggerRow
  type alias for the row tuple it reads.

Zero #[allow] attributes remain in src/ and tests/; clippy is clean,
cargo fmt is clean, and all 254 tests pass.

## 1.0.25

Rustify error types, logging, naming, and structs; fix all clippy warnings.

- error.rs: use thiserror derives instead of manual Display/Error impls
- logging.rs: replace global LOG_TX/LOG_WRITER statics with a LogDb struct
  holding the channel sender and writer task; merge setup_logging and
  setup_log_db into a single logging::setup; clean shutdown without lock-held await
- Rename Python-style functions to idiomatic Rust names:
  get_all_*/get_* -> plain names (known_context_urls, server_info, nodeinfo,
  reply_toots, user_posts, ...), to_post/to_user/to_list -> parse_*,
  build_config -> Config::from_values, PythonFormat -> ConfiguredFormat
- Convert PostRef and RepliedToot from tuple structs to named-field structs
- Fix all clippy warnings (let-else, redundant closures, struct update syntax, etc.)
- Scrub Python references from doc comments; keep them only where they document
  compatibility rationale in inline comments
- Rename tests that referenced Python behavior
- Bump version to 1.0.25

## 1.0.24

Record trigger posts in full, show running runs, and break seen_hosts into columns.

- trigger_posts now records everything a scan pulls in on each post: the
  reply links (in_reply_to_id, in_reply_to_account_id), visibility
  (is_public), whether it was a boost, the author's profile URL, and the
  accounts it mentions. Existing databases get the columns added in place.
- A run is recorded the moment it starts (outcome "running", no end time)
  and the row is completed when it ends, so the dashboard's Recent Runs
  shows the run in progress at the top, with its elapsed time in the end
  time cell, and reloads itself every 5 seconds while it runs. Rows left
  "running" by a dead process are marked interrupted when the web UI
  starts, under the lock so a live process's run is never touched.
- seen_hosts no longer keeps a host's fields in one JSON blob: webserver,
  software, version, the four API supports, last_checked and the failure
  reason are columns (the raw nodeinfo document stays JSON, as its shape
  is the server's own). Databases with the old table are migrated, with
  each row's fields broken out.

## 1.0.23

Record trigger posts, add manual fetch buttons, live log refresh, and -rs branding.

- Record every post a context scan is given in a new trigger_posts table
  (uri, url, acct, replies_count, created_at, source, first/last seen), so
  the posts that generate backfill work are no longer only encountered in
  passing; reply counts are stored instead of discarded.
- Add per-row manual force buttons on the Data page and a POST /fetch
  route: fetch context for a trigger/context post, backfill a remote user,
  re-resolve a seen URL, re-check a host. Opted-out and local accounts are
  refused, private posts are skipped.
- Refresh the Log page in realtime via meta refresh (the UI is no-JS, so
  no SSE), on by default with ?live=0 to pause.
- Brand the page title and header as FediFetcher-rs.
- Rename the Home page run button to FediFetch NOW!.

## 1.0.22

Strip the command line flags and environment variables.

Configuration comes from the database alone now, edited through the web
UI's config page. There are no --server/--access-token/... flags and no
ff_* environment variables: ConfigArgs, args_to_raw, merge and
read_environ are gone, and build_config/build_config_lenient take only
the values stored in the database.

What remains on the command line is operational, not configuration: the
run and serve subcommands (no subcommand still runs once), and serve's
--bind/--port. The database lives in the default state directory, so the
stored state_dir option now only moves the lock file; the XDG_STATE_HOME
convention in default_state_dir is unchanged.

The toml_edit dependency is gone too — nothing reads or writes TOML
anymore.

## 1.0.21

Drop the Logs and Config sub-tabs from the data page.

The data page (/hosts) listed a sub-tab for every table in the database,
including logs and config — data the web UI already shows on its own
dedicated pages (/logs and /config). Those two tables are no longer
sub-tabs; asking for them by name (?tab=logs, ?tab=config) falls back to
the hosts tab like any unknown tab.

The data page test now drills into the runs table instead of logs, seeds
two runs to do it, and asserts the two tables are absent from the tab
row.

## 1.0.20

Make the rate limit cooldown series configurable.

The wait between retries of a rate limited request (HTTP 429) was
hardcoded: every call site passed a 0.5s base backoff that grew fourfold
per step, for five tries. There is now one option, rate_limit_cooldown: a
comma-separated list of seconds, one entry per retry step, tried in
order. When the list runs out, the request fails. The default,
0.5,2,8,32,128, is exactly the series the hardcoded values produced.

The option works everywhere the other options do: the ff_rate_limit_cooldown
environment variable, the --rate-limit-cooldown command line flag, the
database (edited through the web UI's config page), and the config page's
hover tooltip. Entries must be finite, non-negative numbers; an empty list
means no retries at all.

With the series in the configuration, HttpClient::get and post no longer
take max_tries and backoff parameters (as does get_paginated), so the ~25
call sites simply pass the URL and timeout. The x-ratelimit-reset header
still overrides a step's wait when the server sends one.

## 1.0.19

Move all state and configuration into the database.

Every piece of FediFetcher's persistent state now lives in the SQLite
database in the state directory, and the configuration lives there too:

- New tables: config (the configuration, edited through the web UI's
  config page), seen_urls, known_followings, recently_checked_users,
  seen_hosts, context_cache, replied_toots and robots_cache.
- The text state files (seen_urls, seen_hosts, ...) and the robots-*.txt
  cache files are no longer read or written; old state files are not
  imported, so everyone starts fresh with the database.
- The configuration is no longer read from or written to a config file:
  the -c flag, read_config_file and write_config_file are gone, along
  with the toml and xxhash-rust dependencies. Effective precedence is the
  database, then the ff_* environment, then the command line.
- Sessions save with Session::finish (Drop cannot be async), and the
  robots cache is served from the database with a one-day stale sweep.
- The lock file stays a file: it is the only mutex between processes
  (cron and the web UI), which a SQLite write lock would block for the
  whole run. The web UI's run guard remains in-process only.
- .gitignore now covers fedifetcher.db* and /config.json alongside
  /config.toml; AGENTS.md's secrets section points at the database.
- Deleted the dead web_ui_last_run.json state file.

## 1.0.18

Record known context posts in the DB, with a re-check interval.

The posts a run finds around the toots it checks were only remembered
in the seen_urls state file and the log line. They are now recorded in
a known_context_posts table (uri, first_seen, last_seen), which the
data page lists like any other table.

Each run records what it found, and only the posts that need handling
are returned to be resolved: new ones, and already-known ones last
seen longer ago than KNOWN_CONTEXT_RECHECK (a day). The rest the home
server already has, and re-resolving them would only chatter at the
instance hosting them. The "Found N known context toots" line reports
the new/already-known split, so a run that found nothing new says so.

Per-toot context re-fetches were already rate-limited by the
recent_context cache (a minute within the first hour, ten minutes
within the first day, hourly after); this adds the missing database
record and a minimum time between handling an already-known post.

## 1.0.17

Explain the context summary lines with what became of their inputs.

"Found 0 known context toots" and "Added 0 new context toots (with 0
failures)" are logged unconditionally, one per context pass, and a bare
zero says nothing about why: no toots at all, all private, all already
checked recently, all unresolvable, or contexts that came back empty.
The found line now reports what the scan did with its toots (private,
unresolved, already checked recently, fetched, plus context URLs
dropped as local), and the added line reports the URLs it was given and
how many were already seen.

## 1.0.16

Spell out the cause chain in HTTP request errors.

A transport failure — DNS, a refused connection, TLS, a timeout — was
logged as reqwest's "error sending request for url (...)" alone, which
says nothing about why. The error's source chain is now walked into
the message, so a failed request to a server reads, e.g., "Error
requesting <url>: error sending request for url (<url>): client error
(Connect): dns error: failed to lookup address information" instead.

## 1.0.15

Turn the Hosts page into a Data page with sub-tabs for the database.

The /hosts page (now labeled Data in the nav) drills into what
FediFetcher has recorded. Sub-tabs: the hosts from the state files,
as before, plus one for every table in fedifetcher.db — logs, runs,
private_posts, opted_out_accounts — discovered from sqlite_master, so
tables added later appear on their own. Each table tab lists its row
count and shows up to 500 rows, newest first for tables with an id
column, with clickable column headings to sort and flip direction,
like the logs page. SQL NULL renders as a muted NULL; every value is
shown as text, and sorting happens on the column's real type. Table
and column names from the query string are only accepted when they
match names read back from the database, and db.rs quotes and
validates identifiers before interpolating them.

## 1.0.14

Render log levels as colored pills on the /logs page.

The level column showed plain text; it now renders each level as a pill
in the existing badge style, colored by severity: gray for DEBUG, blue
for INFO, amber for WARNING, red for ERROR, with gray as the fallback
for a level the database should never hold. The classes sit alongside
the run-status badges (badge ok/error/running/stopped) in the page's
stylesheet.

Version 1.0.14.

## 1.0.13

Record opted-out accounts; log the skip once, skip quietly after.

An account that has opted out was re-evaluated and re-logged on every
run: "Skipping <acct>: the account has opted out (<reason>)" fired each
time the account surfaced. The skip itself already cost no requests —
it happens before any server is contacted — so what repeated was the
log line. A new opted_out_accounts table (acct, reason, first_seen) in
fedifetcher.db records the account the first time it is found opted
out: the info line fires then, and later runs log the same skip at
debug only. The account is still re-checked against its current data
every run, so one that later opts in (drops nobot, becomes indexable
or discoverable) is fetched after all.

Repeat skips of posts recorded as private are now traceable too: a
known-private post skipped on a later run logs one line at DEBUG
("Skipping private toot ..."), where the default INFO level stays
silent. The first sighting keeps the original "Cannot fetch context of
private toot" line.

Version 1.0.13.

## 1.0.12

Remember private posts in the database; skip them without a request.

A post the home server reports as non-public can never have its context
fetched, yet every run resolved its URL and logged "Cannot fetch context
of private toot" again. The database now records such posts: a new
private_posts table (uri, first_marked) in fedifetcher.db, written the
first time a non-public post is seen and consulted by every later run.

toot_context_can_be_fetched is now async and database-aware: a non-public
toot is marked (and logged) only the first time, and a post marked by an
earlier run is skipped outright. get_all_known_context_urls checks before
resolving the post's URL, so a private post costs no requests at all —
previously find_post ran and its result was discarded. The backfill path
(add_post_with_context) skips private posts before its URL pre-check too.

The SqlitePool is threaded from main/web through run_once into the task
Context and the backfill helpers; database failures degrade to a warning
and the post is processed as before.

Version 1.0.12.

## 1.0.11

Include the version in the web UI startup log line.

The "FediFetcher web UI listening on ..." line now reads
"FediFetcher v<version> web UI listening on http://<addr> (config: ...)",
so a log line identifies exactly which build produced it. Version bumped
to 1.0.11.

## 1.0.10

Add hide_server_in_user_agent to drop the +server part of the User-Agent.

The user agent was always FediFetcher/{VERSION}; +{server}
(https://go.thms.uk/ff), naming the configured server to every remote
server FediFetcher talks to. The new option hide_server_in_user_agent
(default: disabled) drops that part, leaving FediFetcher/{VERSION}
(https://go.thms.uk/ff). It is a full config option: a Config field,
known-option and tooltip entries, coercion in apply, a row on the
config page (a yes/no select, so it saves back to the TOML file), and
the branch in HttpClient::user_agent. Tests cover the string with the
option off and on, the config round trip, the option's row and tooltip
on the page, and saving it through the page. Suite: 204 passed.

Patch bump 1.0.9 -> 1.0.10 per AGENTS.md.

## 1.0.9

Let the API clients speak http, so each flavour has end-to-end tests.

The flavour clients and the home server client built every request URL
with a hardcoded https://, which is right for production but made them
untestable against httpmock (http only): only pure parsing functions
had coverage. Each client now stores a base URL (scheme included) and
gains a with_scheme constructor; new() delegates with "https", so the
production path is unchanged and the http capability exists purely for
tests. Misskey's to_post and notes helper take the base too, so note
URLs built from ids and the context URLs a server returns use the same
scheme as the server they came from. Log messages still name the bare
host, as before.

The URL-shape regexes (post_id_from / username_from) now accept
http:// as well as https://, so a post URL from a local server is
recognized; lookups against that host still go to its API over https.

tests/flavour_http_e2e.rs runs each flavour against a live httpmock
server: Mastodon (user lookup, statuses incl. a boost, context,
an object-URL redirect), Misskey (user search, notes incl. the URL
fallback, children/conversation), Lemmy (account listing, post and
comment listing), PeerTube (account videos, comment threads), and the
home server client (Bearer auth, Link-header pagination across two
pages, verify_credentials). Suite: 201 passed.

Patch bump 1.0.8 -> 1.0.9 per AGENTS.md.

## 1.0.8

Test that every known software name maps to its API flavour.

software_apis recognizes 18 software names across the four flavours
(ten Mastodon-API-compatible, six Misskey-API-compatible, lemmy,
peertube), but only mastodon and sharkey were asserted anywhere, so a
typo in the lists would have silently dropped support for that server
software. The new test walks every name in every flavour's list and
asserts from_nodeinfo detects it as that flavour and no other.

Suite: 196 passed. Patch bump 1.0.7 -> 1.0.8 per AGENTS.md.

## 1.0.7

Make the hosts page's columns user-sortable, like the logs page.

/hosts takes the same ?sort=...&dir=... query string as /logs: the Host,
Software, Version, APIs and Last checked headings are links to sorted
views, clicking the sorted column flips the direction, and an unknown
sort falls back to the default — hosts alphabetically, which is what the
page showed before. Sorting is server-side, so the UI stays
JavaScript-free.

The sort_link helper is now shared by both pages: it takes the page path
and the direction a new column sorts in (descending for /logs, ascending
for /hosts). HostsPage sorts its rows with a stable sort on top of the
alphabetical order, so equal keys keep host order. db::LogSort gains
param()/parse() so the /logs handler and its headings agree on the
column names. The page gains a muted line explaining the sorting, like
/logs has.

Tests seed a seen_hosts state file (entries carry the peertubeApiSupport
marker expire() requires, and last_checked values relative to now) and
cover the default order, ascending and descending by software, by
version, by last checked, the direction indicator, the flip link, and
the fallback for an unknown sort. Suite: 195 passed.

Patch bump 1.0.6 -> 1.0.7 per AGENTS.md.

## 1.0.6

Make the logs page a sortable table, newest first, with a clear button.

The /logs page renders the database's entries as a table (time, level,
message) instead of a preformatted block. Column headings are links to
/logs?sort=...&dir=... — server-side sorting, so the UI stays
JavaScript-free; clicking the sorted column flips the direction, and an
unknown sort falls back to the default. The default view is newest
first (sort=ts, dir=desc).

A "Clear log" button POSTs to /logs/clear, which deletes every row in
the logs table and redirects back to /logs. Lines already on stderr stay
there; the database is what the page reads, and logging itself writes
only to stderr and the database (there is no log file to truncate).

db::recent_logs gains LogSort/LogOrder parameters: the newest N rows are
fetched, then the page is sorted in Rust (stable, so equal keys keep
newest-first order); db::clear_logs empties the table. Tests cover the
sort orders, the fallback, the newest-first default, the table markup,
and the clear button end to end. Suite: 194 passed.

Patch bump 1.0.5 -> 1.0.6 per AGENTS.md.

## 1.0.5

Ignore config.toml, record the version-bump commit rule, bump to 1.0.5.

config.toml holds the real access token. It was never committed (the
token value appears in no commit and no tracked file), but it was not
git-ignored, so a git add -A would have staged it. It is now listed in
.gitignore and git check-ignore confirms it.

AGENTS.md gains two standing rules: every version increment is committed
together with a write-up of the change it belongs to, and config.toml
must never be committed.

Patch bump 1.0.4 -> 1.0.5 per the AGENTS.md convention; full suite green
(187 passed).

## 1.0.4

Update Cargo.lock for the 1.0.4 version bump.

Silence dead-code warnings for the shared test helpers.

Each integration test binary compiles tests/common, but only the http
and backfill suites use LogCapture, so the other binaries warned about
unused helpers. The module now allows dead_code. Patch bump to 1.0.4
per AGENTS.md.

## 1.0.3

Add log-capture test helper, log-line integration tests, and AGENTS.md.

tests/common gains a LogCapture helper: one global tracing subscriber
per test binary with per-thread buffers routed by ThreadId, so
parallel tests can assert on emitted log lines without the flakiness
of set_default. The http and backfill integration suites use it to
cover the new robots.txt, blocklist, and opt-out skip log lines.

AGENTS.md records the repo convention of incrementing the patch
version in Cargo.toml with every change.

Add stop button, debug tab, opt-out logging, and SQLite-backed logs.

Stop button: run_once takes a shared CancelToken checked cooperatively
in the task loop, context fetches, and backfill. The web UI exposes
POST /stop, which sets the token; a running fetch aborts at the next
checkpoint instead of running to completion.

Debug tab: a new web page lets you test the connection to the
configured server and query users and posts through the API, showing
basic results. It deliberately queries the home server directly.

Logging: accounts that opt out are now logged when skipped
("Skipping {acct}: the account has opted out ({reason})"), and startup
warns when respect_robots_txt or respect_user_opt_out is disabled, so
misconfiguration is visible. "Found N known context toots" now lists the
context urls alongside the count.

SQLite: logs and web-triggered run history are stored in
state_dir/fedifetcher.db via sqlx (WAL, busy timeout, one writer task
behind an unbounded channel). The logs page renders rows from the
database and the dashboard shows persisted run history, so both survive
restarts. The old in-memory log ring buffer and the web_ui_last_run.json
sidecar are removed; pruning keeps the newest 100k log rows.

Log when robots.txt or the blocklist stops a request.

HttpClient::request now logs at info level whenever a fetch is skipped:
"Not fetching {url}: its robots.txt disallows it" for robots.txt blocks,
and "Not fetching {url}: {reason}" for blocklist entries, where the
reason names the blocking instance. Previously these skips were silent,
so it was impossible to tell from the log whether a host was ignored
deliberately or never contacted.

Add descriptions for every config option.

option_description(name) returns a short human-readable explanation for
each known config key, sourced from the original FediFetcher docs. The
web UI renders these as title tooltips on the config page. A unit test
asserts every known option has a description so the list cannot drift
out of sync with the config struct.

Report the underlying reason when server or post lookups fail.

get_nodeinfo now returns the failure reason as a string (for example
"prohibited by robots.txt") instead of a bare error, and the reason is
stored in the seen-hosts state and shown on the Hosts page as
"lookup failed: {reason}".

look_up_post likewise returns the reason, so a bad toot URL logs
"Error parsing toot URL {url}: {reason}" with the original cause instead
of a generic message.

Make the lockfile PID-aware so stale locks are reclaimed automatically.

The lock file now records the holder's pid and creation time as JSON.
A lock held by a live process still blocks with a verbose error naming
the pid; locks held by a dead process, in the legacy plain-text format,
or past the lock-hours limit are reclaimed with a warning instead of
forcing the user to delete the file by hand.

Liveness is checked with kill(pid, 0) on unix via the new libc
dependency; other platforms fall back to treating recorded locks as
live for the lock-hours window.

Skip null reblogs in to_post and list toot ids in the timeline count line.

to_post recursed into the reblog field unconditionally; a reblog of null
made every such post surface as "Skipping a mastodon post without a URL
or a date: null". Null reblogs are now filtered out, with a regression
test.

"Found N toots in timeline" now appends the comma-separated post uris
so the log shows exactly which toots were found. The line is built by a
pure helper with unit tests; an empty timeline keeps the old bare line.

Bump version to 1.0.3 and add libc and sqlx dependencies.

Version history since the 8.2.3 port: 1.0.0 is the original Rust port,
1.0.1 the web UI revisions, 1.0.2 the legacy-lockfile reclaim fix, and
1.0.3 the SQLite-backed logs and run history.

libc (unix-only) supports PID liveness checks for the lockfile; sqlx
(runtime-tokio, sqlite) backs the new logs and runs tables.

## Early development (pre-1.0.3)

Add unit and integration test suite.

132 tests covering the port: library unit tests plus integration suites
for the API clients, HTTP and robots.txt handling, notifications,
Link-header paging, run orchestration, and the web UI (pages render
with no <script> tags, the config page masks the access token, and
POST /run records a run).

Port FediFetcher 8.2.3 to Rust, with a no-JavaScript web UI.

Faithful port of the Python FediFetcher at v8.2.3: pulls missing posts
and profiles into your Mastodon instance, from the accounts your users
follow, reply to, list, bookmark, favourite, or get notified about.

- Core library: TOML config, state and persistent store, HTTP client
  with robots.txt support and 429/backoff handling, Mastodon/Lemmy/
  Misskey/PeerTube API clients with Link-header paging, list fetching,
  post backfill, user resolution, translation, and run orchestration
  with lockfile and task tracking
- CLI entrypoint mirroring the Python command line
- Server-rendered web UI (axum + Leptos SSR) with zero client-side
  JavaScript: run, config (access token masked), and status pages
- robots.txt support and the nobot user opt-out check are preserved
  from upstream HEAD and default to enabled
