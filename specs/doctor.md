# Project diagnostics

Implemented boundary, specification 1. CLI help and output schema ownership remains in
[the CLI contract](../crates/barca-cli/docs/contract.md). Check names and provider-specific
identity fields are experimental.

`barca doctor [files...] [--env NAME] [--json|--pretty]` reports prerequisites without
importing pipeline modules, evaluating partition expressions, or executing steps. It uses
the regular project-root/discovery/environment rules. A malformed project configuration
must produce a failed diagnostic rather than hiding all other possible checks.

JSON is an object with `ok: boolean` and `checks: array`. Each check has `name: string`,
`ok: boolean`, `detail: string`, `data: object|null`. `ok` is true exactly when all checks
pass. A failure exits 3 with the usual stderr error envelope; success exits 0. Invalid
arguments exit 2. Human output has one line per check.

Checks cover root, configuration, environment, state mode, local cache writability,
metadata snapshot integrity, the size of `.barca/` against its limit and the free space on its
filesystem (`cache_size`, informational: always `ok`; `data` has `size_bytes`, `limit_bytes`,
`max_size`, `source`, `filesystem_bytes`, `available_bytes`; RFC-0012), the capacity the steps of
a run share (`capacity`, informational: always `ok`; `data` has `capacity`, `pool_size`, `source`;
experimental, #436), source discovery/static extraction, actual Python executable
and matching package version, and configured artifact/shared-state store reachability.
Remote-off skips provider probes. The finite Python probe is stopped and reaped after
30 seconds; malformed output, missing or duplicate store results cannot report success.
No remote object writes, metadata creation or migrations are permitted. The local cache
directory and an immediately removed temporary write probe are allowed.

With a server (`--server [URL]`, or `BARCA_SERVER` / `[server] url` without `--local`), the
report adds server checks after the local ones: `server` (reachable, answers `GET /health`;
when it fails the other server checks are skipped), `server_version` (fails only on another
API version), `server_store` (the server's `store_uri` against this project's `[remote]`),
`server_health` (draining or `metadata_error`), `server_sources` (`load_errors`),
`server_scheduler` (fails when the scheduler stopped on an error or a schedule is `failing` in
`GET /schedule`; paused, `--no-schedule` and `--read-only` pass) and `server_posts` (this
machine's `ledger_posts` rows for that URL in state `unsynced`, read from a snapshot). A failed
check's `detail` ends with a remediation line. Server checks only read: no request changes the
server and no pending post is sent. `--server` with no URL and none configured is a usage error.

Identity lookup uses the selected storage driver's credentials. Report effective identity
when available; report configured context and an explicit unresolved lookup otherwise.
Lookup failure alone does not change a successful store listing into failure. Error hints
use configured context, include recovery, and precede provider errors; they cannot assert
an effective identity after failed authentication. Secret values and signed queries must
not appear in diagnostics. R2/S3-compatible keys and Azure shared keys/SAS are identified
by fingerprints, never raw credential values. Real cloud acceptance remains blocked on
issue #86; controlled tests do not establish real-provider success.

Conformance: [CLI/no-execution controls](../python/tests/test_doctor.py),
[selected-credential adapters and redaction](../python/tests/test_identity.py),
[CLI JSON snapshots](../python/tests/test_cli_contract.py),
[server checks against a real `barca serve`](../python/tests/test_debugging_problems.py), shared helper lifecycle tests
in `helper_proc.rs`.
