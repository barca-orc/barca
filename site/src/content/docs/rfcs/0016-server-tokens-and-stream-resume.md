---
title: 'RFC-0016: Server Tokens and Resumable Run Events'
description: 'Opt-in Barca-issued API tokens, browser token management without accounts, consistent client credentials, and attempt-scoped Last-Event-ID replay.'
---

- **Status:** Under Review; proposed for 0.36.0, not implemented
- **Date:** 2026-10-29
- **Touches:** barca-core | barca-cli | python/barca | HTTP server | UI
- **Related:** [#706](https://github.com/barca-orc/barca/issues/706),
  [#707](https://github.com/barca-orc/barca/issues/707), reopened
  [#187](https://github.com/barca-orc/barca/issues/187).
  Amends [RFC-0007](/rfcs/0007-server-mode/) and
  [RFC-0004](/rfcs/0004-http-server-api/).

## 1. Summary and decisions

The user has chosen opt-in tokens issued and validated by Barca, with UI token management
and no user-account system. This deliberately reopens the built-in-auth non-goal for this
bounded scope. Accounts, passwords, SSO/OIDC, per-user ownership and a roles system remain
outside it. Existing unauthenticated servers and externally authenticated proxies remain
the default.

This RFC proposes the remaining details: one-time local bootstrap, project-wide token
records, browser bearer credentials, revocation of live streams, and bounded event replay.
Those details are not shipped or already approved merely because token ownership is decided.

`BARCA_SERVER_TOKEN` supplies bearer credentials to every server request. URL userinfo stays
supported for Basic-auth proxies. Run streams resume after the last completely processed
event, using an attempt-scoped `Last-Event-ID`, rather than replaying and skipping a count.

## 2. Motivation and current implementation

The proxy deployment in #706 needs consistent credentials on health, inspection, execution,
ledger requests, cancellation and SSE. Credential masking shipped in 0.35.2; sending and
validating bearer tokens has not shipped.

Today `RunChannel` retains events and assigns numeric IDs, but the events handler always
subscribes from zero. The CLI already reconnects for up to 60 seconds, checks the server's
`started_at`, and skips a count of replayed events. Its parser discards IDs. The UI uses
native `EventSource`, which cannot supply the proposed Authorization header. Reconnect
must distinguish an interrupted connection from a new attempt of the same accepted run.

## 3. Operator and browser flow

```toml
[server]
auth = "token"
```

`BARCA_SERVER_AUTH=token` overrides TOML; `barca serve --auth token` overrides both. The only
values are `off` and `token`, default `off`. Invalid values are usage errors (exit 2).
Turning auth off is explicit configuration, never a response to unreadable credential state.

The server creates an owner-only project auth directory and starts locked. Startup explains
how to bootstrap but prints no secret. On the deployed host, in the deployed project:

```bash
barca tokens bootstrap
```

This local-only command prints one bootstrap code to stdout, with no surrounding prose.
It creates one short-lived grant, replacing any previous unconsumed bootstrap grant. It is
not a general API token and expires after 15 minutes. Normal diagnostics never reproduce it.
The command is an explicit secret-output operation; it does not send the code to a browser,
open a URL, or put it in command arguments.

The UI's Connect screen accepts a bootstrap code or an existing API token. Exchanging a
bootstrap code consumes it atomically and returns a bearer token expiring after 12 hours.
The user can then create named API tokens, copy a token once, list token metadata, and revoke
tokens. A machine or agent uses a generated token through its environment:

```bash
BARCA_SERVER_TOKEN="$DEPLOYED_BARCA_TOKEN" barca history --server https://barca.example/project
```

Ordinary API tokens have the existing full server capabilities, constrained by the server's
read-only mode. They can also manage tokens. A name labels a credential; it is not an account
or owner. Optional expiry is an absolute UTC timestamp; omitting it creates a non-expiring
token. The UI states this before creation. Local bootstrap remains the recovery mechanism
if every usable token is revoked or lost.

## 4. Credential boundaries

### 4.1 Client transport

Use a server-specific credentials helper shared by CLI requests and core ledger transport.
Do not add credentials to the general-purpose HTTP client used for arbitrary destinations.

- Resolve `[server] url`, `BARCA_SERVER` and `--server` with existing precedence and prefix
  handling. URL userinfo is percent-decoded and sent as Basic credentials on every request.
- Read `BARCA_SERVER_TOKEN` as a secret string. A present empty value, surrounding whitespace,
  control characters or an invalid header value is a usage error, without echoing the value.
- Send `Authorization: Bearer ...` on every request to that server: initial health and store
  checks, graph/status/history, run submission, follow/reconnect, output/logs, DELETE,
  and ledger lookup/publication/retry. The deprecated Python Client has the same transport
  behavior until removal.
- Reject simultaneous URL userinfo and bearer credentials as a usage error. One HTTP
  Authorization header cannot authenticate both a Basic proxy and Barca's bearer mode.
  A layered proxy must forward bearer credentials and use a different outer-auth mechanism.
- Do not forward credentials to another origin on redirects. Preserve a configured path
  prefix, reject redirects escaping it, and never forward secrets while probing arbitrary URLs.
- Token rotation does not change ledger post identity or resubmit an already accepted run.
  Do not persist bearer secrets in metadata, pending requests, run events or telemetry.
- Errors, nested envelopes, reflected proxy response text, tracing and diagnostics redact
  known bearer and Basic credential material. Keep 0.35.2's URL-userinfo masking. Invalid
  credentials must not be printed as part of a configuration/header error.

URL credentials keep their existing local URL-identity compatibility. The implementation
must document where historical userinfo-bearing configuration/metadata can contain them;
this RFC does not claim an automatic scrub of old databases or log files.

Use HTTPS outside loopback, with TLS terminated by the deployment's proxy where appropriate.
Barca does not terminate TLS or prove that an externally configured proxy is secure. Do not
put API or bootstrap tokens in URLs, query strings, fragments or committed configuration.
Bearer transport follows the security model in [RFC 6750](https://www.rfc-editor.org/rfc/rfc6750):
possession grants access, so transport and storage must protect the token.

### 4.2 Server authorization

In token mode, authorize before executing API handlers, reading project data, or attaching
inspection ETags. `/health`, GET/HEAD inspection, ledger traffic, cancellations and event
connections all require a valid bearer token. Do not exempt read-only servers or health probes.
The operator must configure authenticated health checks.

The only unauthenticated reads are the exact static UI routes/assets and
`GET /auth/status`, which returns only `{ "mode": "off" | "token" }`. The bootstrap exchange
has its own single-use credential check. No fallback, encoded path, trailing-slash variant,
method override, HEAD request or conditional response may bypass API authentication.

Missing, unknown, revoked or expired credentials return 401 with `WWW-Authenticate: Bearer`
and a sanitized existing error envelope (`kind: infra`, `code: 3`, reason
`authentication_required` or `invalid_token`). Authorization does not grant capabilities
forbidden by read-only mode. Authentication failures are terminal for CLI follow/reconnect;
they do not consume a 60-second retry loop or trigger another POST.

Auth-off mode preserves the deployed API and proxy behavior. Token management routes then
reject requests rather than unexpectedly creating an unauthenticated administration API.

### 4.3 Browser credentials and CSRF

Use bearer headers for the browser too. Do not implement cookies, password login, or an
ambient browser session in this version. The UI keeps its connected token in memory and
tab-scoped session storage, keyed by server origin and configured API prefix. Reloading
the tab preserves the connection; Disconnect clears it. No localStorage, URL or analytics
storage receives a token. Token mode is intended for same-origin UI/API deployment.

Every UI fetch, including its stream reader, adds Authorization explicitly and uses
`credentials: omit` in token mode. The server accepts no token cookie. It enables no
cross-origin credential CORS policy, reflects no arbitrary Origin, and permits no
state-changing GET or form-based token operation. Bootstrap and token creation require
JSON requests, not simple form content types. Cross-origin browser preflights are rejected.
These choices remove ambient Barca credentials from CSRF requests; a later cookie design
would require a separate CSRF design. SameSite alone would not replace that protection,
as explained by [OWASP's CSRF guidance](https://cheatsheetseries.owasp.org/cheatsheets/Cross-Site_Request_Forgery_Prevention_Cheat_Sheet.html).

Session storage is readable by scripts on the origin. An XSS or compromised UI can use an
active credential; this is an explicit tradeoff against adding a cookie/session subsystem.
Use separate origins for mutually untrusted projects; a URL prefix is not browser-origin
isolation. Token responses use `Cache-Control: no-store`, and the Connect/token UI sends no
credential data to external resources or telemetry. Show generated secrets only until the
user closes their creation panel; lists expose metadata, never a retrievable secret.

## 5. Issuance, storage and revocation

Store project-wide auth state under `.barca/auth/`, independently of per-environment metadata.
Directories are owner-only and files owner-readable/writable. Auth state has its own version,
atomic replacement and cross-process lock; malformed/unreadable state fails closed. No token
state is stored in the shared metadata backup. Restoring metadata must not resurrect revoked
tokens. Operators must preserve the auth directory separately when migrating a server.

Generate secrets with the OS CSPRNG: at least 256 random bits, encoded without whitespace.
Use distinguishable versioned API-token and bootstrap formats, with an opaque public ID for
lookup. Store only domain-separated cryptographic digests and metadata, never the original
secret. High-entropy random tokens are not user-chosen passwords. Validate the secret with
a constant-time comparison. Do not build JWT verification, signing keys, OAuth or accounts.

Public token metadata: `id`, `name`, `created_at`, nullable `expires_at`. Names have at most
128 UTF-8 bytes and no control characters. Creation accepts a name and optional future expiry;
it returns metadata and the secret once. List never returns secrets or digests. Revoke is
idempotent; unknown/revoked IDs reveal no secret. Bound active records to 1000 per project,
returning a conflict with remediation if full. Consumed/expired bootstrap grants are discarded.
Do not write `last_used` on every request or turn authentication into another metadata writer.

Proposed HTTP surface (experimental; implementation must update OpenAPI and conformance):

| Method and route | Credential / result |
|---|---|
| `GET /auth/status` | Public mode only |
| `POST /auth/bootstrap` | JSON bootstrap code; consume once, return 12-hour token |
| `GET /auth/tokens` | Valid API token; metadata list |
| `POST /auth/tokens` | Valid API token; create named token, show secret once |
| `DELETE /auth/tokens/{id}` | Valid API token; idempotent revoke, 204 |

These administrative writes affect only auth state and are permitted with a read-only
ledger server. That exception must be explicit in help/docs; pipeline mutations still fail.

A successful same-process revoke invalidates validation caches and closes affected streams
before acknowledging. Cross-process changes to the project auth directory are observed
within one second, including while a stream is idle; expiry also closes streams. Recheck
authorization before sending subsequent frames. Already transmitted or proxy-buffered bytes
cannot be recalled. No revocation waits for another event to wake an idle stream.

## 6. Resumable events

### 6.1 Cursor and replay

Each accepted run attempt has a random 128-bit stream epoch. A recovered/requeued attempt
of the same public run ID gets another epoch. Each data event has ID `<epoch>:<sequence>`,
where epoch is 32 lowercase hex characters and sequence is a monotonically increasing,
one-based decimal integer. IDs are stable while that attempt lives; never reuse one for
different data. The run handle and event epoch have different purposes.

A request without `Last-Event-ID` asks for the stream from its beginning. A request with a
valid cursor starts strictly after that event. Retain a bounded serialized replay window:
at most 4096 events or 16 MiB per run, and 64 MiB across replay windows, whichever applies
first. Evict oldest retained events first, including from active runs if required. Finished
runs retain replay only while their existing age/memory eviction permits it. Do not create a
durable event archive, copy full backlogs per subscriber, or add an unbounded slow-client queue.

An oversized event may make replay discontinuous: advance the sequence, mark the missing
range, and signal expiration rather than silently skipping it. A subscriber that falls
behind the retained window is closed; reconnect follows the same expiration rules. The
canonical run outcome and persisted log APIs remain available independently of replay.
Replay-byte bounds do not claim to bound all outcome, HTTP-buffer or worker memory.

| Condition | HTTP behavior before stream headers |
|---|---|
| Retained cursor in this epoch | 200 SSE, events strictly after it |
| Last event was terminal and already consumed | 204; client reads canonical outcome |
| Malformed/overlong cursor or sequence beyond emitted data | 400, `invalid_event_cursor` |
| Cursor belongs to a different attempt | 409, `event_cursor_mismatch` |
| Earlier data is no longer retained, including a first connection after truncation | 410, `event_cursor_expired` |
| No live channel | Existing 404; inspect `GET /runs/{id}` |
| Invalid credential | 401, before disclosing cursor/channel information |

Limit cursor headers to 128 bytes and check integers for overflow. A cursor is never an
authorization credential. Do not echo arbitrary cursor content into diagnostics. Replay and
live subscription share one cursor owner so an event emitted during reconnect cannot fall
between the replay snapshot and subscription. Heartbeat comments have no data event ID.

The [HTML SSE standard](https://html.spec.whatwg.org/multipage/server-sent-events.html)
defines `Last-Event-ID` as a reconnect header. Its text framing remains intact: UTF-8 chunk
boundaries, CR/LF variants, multiline data, comments and field parsing must not damage IDs.
The SSE JSON `RunEvent` payload remains owned by the existing event protocol.

### 6.2 Client behavior

Advertise `capabilities.event_resume: 1` in authenticated `/health`. A new client uses this
cursor protocol only when advertised. With an older server that omits it, retain the legacy
replay/count behavior and its limitations rather than sending new cursors that it ignores.
The capability is additive; this does not renumber the existing run JSON API version.

Replace the CLI's replay-count skipping with the last completely parsed and successfully
handled event ID when talking to a capable server. Keep the existing configurable reconnect window and backoff. Discard a
partial frame after a drop and resume from the last complete event. A resumed frame is not
printed twice, and an event not handled is not acknowledged by advancing the cursor.

Network errors, transient proxy failures and EOF before a terminal event can reconnect.
401/403 stop immediately. On 404/204, read the existing run's canonical outcome; if it is
still pending/running, continue bounded reconnection. On epoch mismatch, inspect that run:
follow a new live/requeued attempt with an explicit restart message and a fresh cursor,
never by creating another run. On expiration, retrieve a terminal outcome if available;
otherwise exit 3 with a replay-gap message and a pointer to run history/logs. Do not silently
claim complete log delivery or resubmit work to repair an event gap.

The UI replaces native EventSource with a fetch stream so credentials, cursor headers and
HTTP errors use the same transport as every other request. Preserve reducer/state behavior
on ordinary reconnects. On expired replay, show an explicit gap, switch to bounded run-view
polling, and offer recorded logs; never represent a truncated stream as complete. Disconnect,
revocation, expiry, terminal completion and component teardown abort the stream and its
reconnect/poll timers. The CLI's signal-driven cancellation still sends authenticated DELETE.

Old numeric IDs are not accepted as new attempt-scoped cursors. Older clients that do not
send cursors can still attach from the beginning while it is retained. Document new cursor
errors and bounded replay as 0.36.0 upgrade behavior; auth remains opt-in.

## 7. Verification and implementation boundaries

- Configuration/help/examples and the embedded manual cover auth mode, local bootstrap,
  secret-output behavior, proxies, prefixes, health checks, read-only administration and
  `BARCA_SERVER_TOKEN`. Update the matching site references and CLI snapshots together.
- One credential helper owns every Rust server request; test flag/TOML/environment URL
  userinfo, bearer parity, conflicting settings, redirects, reflected errors and legacy
  Python Client behavior. Workers and artifact-store clients receive no server credentials.
- Exercise the full router in auth-off/token modes: every method, HEAD/ETag, encoded paths,
  UI/static exceptions, health, ledger, SSE and cancellation. Test 401/403 envelopes and no
  mutation or project-data disclosure before authorization.
- Test one-time bootstrap races, expiry/clock changes, atomic state writes, permissions,
  malformed state, token rotation, restart, metadata restore independence, and revocation
  of active/idle streams across processes. Use injected private clocks, not public test knobs.
- Browser tests cover Connect, reload/disconnect, one-time secret display, creation/list/
  revoke, no URL/cookie/telemetry credential exposure, blocked cross-origin requests and
  navigation/stream cleanup. Authentication must add no account registration flow.
- Stream tests cover new-client/old-server capability fallback, replay/live races,
  drops between frames and inside UTF-8/CRLF frames,
  exact-once rendering within a retained attempt, terminal reconnect, epoch changes,
  malformed/future/expired cursors, slow subscribers, oversized events and bounded retention.
  A proxy drops real authenticated streams in integration tests without duplicating tasks.
- Benchmark small authenticated requests and reconnects, and large-event retention. Add no
  per-request token-file walk, metadata hash or write. No changes to worker/transfer wire
  formats, cache keys, asset freshness or Python import rules are needed.
- Authentication and resume do not resolve #706's artifact hashing/remote probe cost. That
  work remains a separate 0.36.0 requirement before publication. Experimental server-mode
  docs must state these remaining deployment/stability limits rather than imply readiness.

## 8. Alternatives and review points

Proxy-owned issuance/validation was considered; the user chose Barca ownership. Token-file
configuration without UI issuance does not meet that decision. Accounts and OAuth add a
different product and remain excluded.

HttpOnly cookie sessions could reduce token exposure to page scripts, but require cookie
scope, CSRF tokens, login/logout and session lifecycle. This proposal chooses tab-scoped
bearer access and documents its script-storage tradeoff. Review that choice before acceptance.
Also review shared full-access token management, non-expiring API tokens, read-only auth
administration, the one-second cross-process revocation bound, and explicit replay expiration.
The defaults, routes and CLI names above are proposals until this design merges.
