# Per-process storage overrides (#309, #310, P05)

## Contract and precedence

`BARCA_REMOTE=off` disables Barca-managed remote artifact and metadata access for
one process, across CLI execution, inspection and serve. Unset or empty keeps
existing resolution unchanged. Any other non-empty value is a usage error; no
new CLI flag, TOML policy or per-node setting is added. Existing `--env` >
`BARCA_ENV` > `default_env` > `default` chooses the local environment unchanged.

Explicit remote-off overrides all remote URIs, state mode, transfer settings and
storage options from environment/TOML. It resolves local artifacts and local
history, skips remote-option validation, and launches no remote transfer/state
helper. TOML syntax/unknown keys are still validated normally.

`BARCA_STATE=off` remains the narrower existing override: metadata stays local,
while configured artifact storage remains active. Serve continues refusing
optimistic shared state; use this existing state-off spelling to serve with
remote artifacts, or remote-off for fully local operation. No silent downgrade.

## Existing history and artifact reads

Switching off does not delete or rewrite history. Recorded remote/shared-store
locations remain truthful historical facts, but do not become eligible cache
inputs or inspection/SQL reads. Cache lookup selects the newest matching local
materialization; if no local result is recorded, execution computes it locally.
Later calls reuse that local cache normally. Do not fetch an old remote result
just because its row remains in local history. Explicit user code doing network
I/O is outside this storage override.

Use one resolved internal flag and a shared local-artifact predicate. Apply the
predicate to cache lookup, status shape/schema reads, and SQL view selection.
Reuse existing no-record/missing-result behavior rather than introduce new
public status keys/reasons or privacy policy.

## Delivery and verification

1. Implement resolver precedence and tests covering off/unset/empty/invalid,
   named environments, explicit URI/state conflicts, legacy literal artifact
   overrides, and ignored remote-only options when off.
2. Scope cached/inspection artifact reads to local paths when explicitly off.
   Test existing remote history, a prior local result behind a newer remote row,
   repeat local caching and partitioned SQL candidates without changing history.
3. Exercise actual CLI and serve against deliberately failing remote helpers:
   remote-off never contacts them; state-off keeps artifact sharing, optimistic
   serve still exits 2, empty remote variable retains configured remote behavior.
4. Update help examples, manual, configuration reference, README and CLI contract;
   regenerate affected snapshots and run config/core/CLI plus targeted Python
   execution/inspection/server/contract tests. One bounded shared-config PR.

This implements the process override slices only. Concurrent shared-state serve,
remote discovery and per-node privacy/cache policy remain separate designs.
