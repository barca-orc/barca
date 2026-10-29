# Project token authentication

## Scope and status

Specification version: 2. Status: implemented for pending 0.36.0.
Scope: token storage, bootstrap, request authentication, administration and browser access.
Stream cursor resume remains accepted intent in [RFC-0016](../../site/src/content/docs/rfcs/0016-server-tokens-and-stream-resume.md).
Exact HTTP shapes belong to [OpenAPI](../interfaces/server-api.openapi.yaml); client
credential rules belong to the [manual](../../crates/barca-cli/docs/server-mode.md).

## Entities and state

One project has an auth mode (`off` by default or `token`), at most 1000 ordinary API tokens,
one bootstrap connection and one unconsumed bootstrap grant. Credentials have public IDs,
names, creation times and optional expiries. They represent full project capabilities, not
users or roles. Read-only ledger mode remains an independent restriction.
Auth state lives in `.barca/auth/` under the project root, shared across environments and
separate from backed-up metadata. It has a version and owner-only directory/files.

## Rules and transitions

- The local `tokens bootstrap` command replaces the unconsumed 15-minute grant and prints
  its secret only to stdout. Redemption atomically consumes it and replaces the 12-hour
  bootstrap connection. Ordinary API tokens survive recovery.
- Any valid API or bootstrap connection token can list metadata, create named API tokens
  and revoke any token, including itself. Names are 1–128 UTF-8 bytes without controls.
  Omitted/null expiry is non-expiring; a supplied expiry must be in the future.
- Creation reclaims expired ordinary tokens before enforcing the limit. The independent
  bootstrap connection slot keeps recovery available when all 1000 ordinary slots are full.
- Token mode authenticates every API method before route handling or ETag computation.
  Only exact static UI reads, `GET /auth/status` and the credential-bearing bootstrap
  exchange are public. Cookies, method overrides and unknown UI paths grant no access.
- Each operation locks and reloads auth state. Mutations persist through atomic replacement
  and file/directory sync before acknowledgement. Validation never records last-used state.
- Local revocation notifies open SSE responses; idle responses also revalidate at one-second
  intervals for external revocation and expiry. Invalid credentials terminate the response.

### Browser connection

Connect accepts a bootstrap code or an existing token. A successful connection keeps only
its API/connection token in memory and tab session storage keyed by origin and API prefix.
Disconnect clears credentials and query data before showing Connect; it does not revoke.
Authentication failure clears the current connection. A late failure for an older token must
not erase a replacement credential. Creating a token shows its secret only in that page's
transient state; reload does not recover the secret. The form explicitly states no expiry
when no future expiry is chosen. All tokens grant full project/token-management capabilities.

Token-mode fetches omit cookies. Browser request URLs stay within this origin and project
prefix, and redirects are rejected. Fetch-based SSE shares these credentials; closing a view
aborts its reader and reconnect timer. Same-origin script access to session storage is an
accepted tradeoff; untrusted projects require separate origins.

## Invariants

- **INV-AUTH-SECRET:** issue at least 256 bits of OS randomness; persist only domain-separated
  SHA-256 digests and public metadata; compare credential digests in constant time.
- **INV-AUTH-RECOVERY:** a full ordinary token list cannot prevent local bootstrap recovery;
  one grant has at most one successful redemption.
- **INV-AUTH-ORDER:** API authentication precedes project reads, mutations and conditional
  responses. Auth-off mode never enables unauthenticated token administration.
- **INV-AUTH-SEPARATION:** metadata backup/restore cannot create or resurrect a credential.
  Project authentication is independent of the active metadata environment.

## Failures and recovery

Malformed, unreadable or unknown-version state fails closed. Token mode startup fails if
state cannot be opened; runtime reads reject credentials when state becomes unavailable.
No fallback to auth-off is allowed. Symlinked auth directories, state and lock files are
rejected. Failed mutations do not reveal a secret or intentionally overwrite corrupt state.
Restoring only metadata does not restore access; preserve auth separately for migration.

## Acceptance and evidence

[Store tests](../../crates/barca-server/src/auth.rs) cover digest-only persistence,
permissions, one-use bootstrap/replacement, capacity recovery, expiry, concurrent independent
stores, revocation and corrupt/symlink state. [Router tests](../../crates/barca-server/tests/auth.rs)
cover method/path authorization, ETag ordering, read-only administration, revocation,
secret-safe errors and OpenAPI response schemas. [Stream tests](../../crates/barca-server/src/auth_http.rs)
cover idle local/external revocation and expiry through the real response middleware.
[Browser tests](../../ui/e2e-auth/connection.spec.ts) drive real bootstrap, authenticated task
streaming, token creation, one-time display, API-token connection, reload, disconnect and
revocation. [Credential tests](../../ui/src/lib/credentials.test.ts) check scope, headers,
redirect refusal and storage failures; [stream tests](../../ui/src/lib/eventStream.test.ts)
check incremental decoding, abort and terminal authentication failure. These do not prove
TLS deployment or immunity to same-origin scripts.

## Open questions and gaps

[CLI/server integration](../../python/tests/test_server_tokens.py) exercises project-root bootstrap,
real redemption, issued-token execution, Python client access and revocation. Attempt-scoped
bounded replay remains unimplemented. Full release conformance must pass before 0.36.0.
