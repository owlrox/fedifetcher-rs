# Compatibility Coverage: Conformance Harness + Generated Matrix

## Problem Statement

How might we turn fedifetcher-rs's per-software support from a name allowlist
with no test evidence into a provable, always-current claim — so gaps surface
as CI failures instead of user bug reports?

Today `software_apis()` in `src/servers.rs` claims support for ~20 server
kinds across four flavors, but nothing verifies those claims against real
servers. When a fork changes its API (Iceshrimp.NET did during beta), we find
out from users. Knowledge about *why* specific software is unsupported lives
in dead links and issue threads (nanos/FediFetcher#143), not in the repo.

## Recommended Direction

Two builds, one goal — internal safety:

1. **Live conformance harness** (manual-trigger CI job) that runs the
   fetcher's actual read paths — resolve status URL → fetch context, resolve
   actor, paginate — against one demo instance per flavor, read-only, tiny
   request volume. A per-software regression becomes a red build, not a bug
   report.
2. **Generated compatibility matrix** (`docs/COMPATIBILITY.md`) derived from
   `software_apis()` and enriched with last-verified dates from harness runs.
   Drift between code and doc fails CI.

This is tooling for *maintaining* support coverage, not for adding new
software. New-software candidates (Mitra, Piefed, Wafrn, Hollo, snac, NeoDB)
are tracked in the appendix below with their blockers.

## Software Inventory (from nanos/FediFetcher#143 + repo state)

### Supported — Mastodon flavor

| Software | Docs | Source |
|---|---|---|
| Mastodon | https://docs.joinmastodon.org/api/ | https://github.com/mastodon/mastodon |
| Pleroma | https://docs.pleroma.social/ | https://git.pleroma.social/pleroma/pleroma |
| Akkoma | https://docs.akkoma.dev/stable/ | https://akkoma.dev/AkkomaGang/akkoma |
| Hometown | https://github.com/hometown-fork/hometown | https://github.com/hometown-fork/hometown |
| Pixelfed | https://docs.pixelfed.org/ | https://github.com/pixelfed/pixelfed |
| Iceshrimp / Iceshrimp.NET | https://iceshrimp.dev/iceshrimp/Iceshrimp.NET | https://iceshrimp.dev/iceshrimp/iceshrimp |

### Supported — Misskey flavor

| Software | Docs | Source |
|---|---|---|
| Misskey | https://misskey.io/api-doc | https://github.com/misskey-dev/misskey |
| Firefish (ex-Calckey) | https://joinfirefish.org/ | https://git.joinfirefish.org/firefish/firefish |
| Sharkey | — | https://git.joinsharkey.org/Sharkey/Sharkey |
| Foundkey | — | https://akkoma.dev/FoundKeyGang/FoundKey |

### Supported — Lemmy flavor

| Software | Docs | Source |
|---|---|---|
| Lemmy | https://lemmy.readme.io/ | https://github.com/LemmyNet/lemmy |

### Supported — PeerTube flavor

| Software | Docs | Source |
|---|---|---|
| PeerTube | https://docs.joinpeertube.org/api-rest-reference.html | https://github.com/Chocobozzz/PeerTube |

### Not supported — blockers documented

| Software | Blocker | Reference |
|---|---|---|
| GoToSocial | Auth required for all client API reads; maintainers declined unauthenticated public reads by design | https://codeberg.org/superseriousbusiness/gotosocial/issues/2038 |
| Friendica | Mastodon-compatible API exists but context returns 404/403 unauthenticated; numeric-vs-UUID status ids | https://wiki.friendi.ca/docs/api-mastodon |
| Wafrn | 401 on all Mastodon-layer reads; obfuscated status ids not exposed in post URLs | https://github.com/gabboman/wafrn |
| Hollo, snac, NeoDB | Tiny user bases; auth required for reads | https://github.com/dahlia/hollo, https://codeberg.org/grunfink/snac2, https://github.com/neodb-social/neodb |
| Mitra | Pleroma-flavoured API, not yet implemented | https://codeberg.org/silverpill/mitra |
| Piefed | Lemmy-like, unimplemented | https://codeberg.org/rimu/pyfedi |
| WordPress + ActivityPub plugin | Outbox pagination only; no context API | https://github.com/Automattic/wordpress-activitypub |
| Meisskey | No public API to fetch missing posts | — |

## Key Assumptions to Validate

- [ ] NodeInfo `software.name` reliably discriminates forks (Sharkey vs
      Misskey vs Cherrypick report distinct names; verify per flavor).
- [ ] "Mastodon-compatible" forks keep context-endpoint semantics — the
      harness exists precisely to catch drift here.
- [ ] Demo instances tolerate scheduled read-only polling without rate-limit
      blowups (keep volume to a handful of requests per run).

## MVP Scope

- `#[ignore]`-gated conformance tests, one per flavor, exercised via
  `cargo test -- --ignored` in a manual-trigger CI job against pinned demo
  instances.
- `docs/COMPATIBILITY.md` generated from `software_apis()` + harness results,
  with last-verified dates per software.

## Not Doing (and Why)

- **New software support (Mitra, Piefed, Wafrn, …)** — the harness must exist
  first, or new support ships unverified. Coverage tooling precedes coverage.
- **Public status page** — internal safety tool only; publish later if useful.
- **Unknown-server telemetry** — a discovery feed for new forks, but it needs
  opt-in infrastructure; deferred.
- **Auth-walled scraping** — GoToSocial/Friendica stay blocked at their
  maintainers' request, not worked around.

## Open Questions

- Which demo instances to pin? Self-hosted (full control, maintenance cost)
  vs. volunteer-run demo servers (zero maintenance, flake risk).
- Should the harness run on every CI push (fast, network-dependent) or only
  on manual trigger / nightly schedule?
