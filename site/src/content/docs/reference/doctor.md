---
title: Doctor and local debugging
description: Check project prerequisites, stores and credential identity without running steps.
---

# Doctor: check a project before running it

```bash
barca doctor
barca doctor pipeline.py --json
barca doctor src/ --env local --pretty
```

`doctor` reports the project root and how it was found, selected environment, parsed
configuration, actual Python executable and package version, local cache writability,
metadata integrity, discovered source files and parse errors. It checks configured artifact
and shared-state stores with read/list operations. It runs no steps, imports no pipeline
modules and does not evaluate dynamic partition expressions. Source checks are syntax and
decorator extraction checks, not a complete execution plan.

With `--pretty`, each check occupies one line. With `--json`, stdout is
`{"ok": true, "checks": [{"name": "...", "ok": true, "detail": "...", "data": {}}]}`.
The default follows the usual terminal/pipe output choice. Exit 0 means every check passed;
exit 3 means at least one prerequisite failed, with the normal error envelope on stderr.
Argument errors still exit 2. Check names and provider-specific `data` are experimental.

The Python probe has a 30-second overall deadline. It lists stores and looks up the
identity behind the selected credentials where supported: AWS STS caller, Azure token
principal/tenant, Google credential email or token identity, and an access-key fingerprint
for R2 and other S3-compatible endpoints. An unavailable identity lookup is reported as
unresolved; a successful store listing still passes. Azure shared-key/SAS credentials use
fingerprints. Fingerprints identify a credential, not a person. Credentials and signed URI
queries are redacted. Authentication errors show configured account/tenant context and a
recovery command before the provider error; a failed login cannot establish the effective
principal.

No remote objects are created or changed. Checking the local cache can create its directory
and a temporary write probe, removed immediately. Existing metadata is validated from a
snapshot without migrating or changing the original; an absent database is reported and
left absent. Store reachability does not establish upload permission or validate user input
paths inside steps.

## Local-only debugging profile

Run this block from your project root. Use local input fixtures in your pipeline/config;
these overrides disable remote artifact/state synchronization, not application-level I/O.
The `local` environment uses a separate cache namespace.

```bash
export BARCA_REMOTE=off
export BARCA_STATE=off
export BARCA_ENV=local
barca doctor --pretty
barca get --refresh-all
```

`--env` overrides `BARCA_ENV` for an individual command. See `barca docs remote` for store
configuration and `barca docs discovery` for project roots and source selection.
