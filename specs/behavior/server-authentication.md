# Project token authentication

## Scope and status

Specification version: 1. Status: implemented for pending 0.36.0.
Scope: server-side token storage, bootstrap, request authentication and administration. Browser Connect/token management and
stream cursor resume remain accepted intent in [RFC-0016](../../site/src/content/docs/rfcs/0016-server-tokens-and-stream-resume.md).
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
These tests do not prove browser behavior or TLS deployment.

## Open questions and gaps

[CLI/server integration](../../python/tests/test_server_tokens.py) exercises project-root bootstrap,
real redemption, issued-token execution, Python client access and revocation. UI credential
handling/token management and attempt-scoped bounded replay are not yet implemented. Full
release conformance must pass before 0.36.0.
