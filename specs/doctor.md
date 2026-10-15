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
metadata snapshot integrity, source discovery/static extraction, actual Python executable
and matching package version, and configured artifact/shared-state store reachability.
Remote-off skips provider probes. The finite Python probe is stopped and reaped after
30 seconds; malformed output, missing or duplicate store results cannot report success.
No remote object writes, metadata creation or migrations are permitted. The local cache
directory and an immediately removed temporary write probe are allowed.

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
[CLI JSON snapshots](../python/tests/test_cli_contract.py), shared helper lifecycle tests
in `helper_proc.rs` and `state_sync.rs`.
