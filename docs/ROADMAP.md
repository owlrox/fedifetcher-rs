# Roadmap


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

