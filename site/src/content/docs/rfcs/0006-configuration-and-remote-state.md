---
title: 'RFC-0006: Configuration & Shared Remote State'
description: 'barca.toml, env var/CLI precedence, --env separation, and the optimistic shared-state sync protocol.'
---

- **Status:** Accepted (retroactive baseline — documents behavior as of v0.6.1)
- **Date:** 2026-07-16
- **Touches:** barca-core | barca-cli
- **Supersedes / Related:** [RFC-0005](/rfcs/0005-artifact-serialization-and-storage/) (content-addressed artifacts this config points at), [RFC-0004](/rfcs/0004-http-server-api/) (`serve`'s shared-state restriction)

---

> **Amended (0.13, issue #202):** `barca.toml` is now discovered by walking up from the
> cwd, and barca changes into that directory (the project root) before running, so
> `.barca/` stays anchored to the same place as its config. The cwd-only rule below
> described 0.5 to 0.12.

> **Amended (after 0.17.0, issue #221):** §4.1 now defines what a pull does to the local
> database, in "What a pull does to the local database". A pull used to replace only the
> main database file and leave the old write-ahead log beside it, which could drop other
> machines' history at the next push; and since 0.17.0 a run records its finished steps
> locally before its push. "Nothing is lost" now also covers local rows that were never
> pushed, a killed run's included.

## 1. Summary

Configuration resolves through three layers — **CLI flag > environment variable >
`barca.toml` > built-in default** — discovered in the current working directory only.
`--env`/`BARCA_ENV`/`default_env` separates cache, artifacts, and shared remote state
per named environment. When `[remote].uri` is set, barca shares both artifacts and the
metadata DB across machines via an optimistic pull/checkpoint/push protocol.

## 2. Motivation

Barca persists everything under `.barca/` relative to the invocation directory (per
[Core Constraints](/core-constraints/)'s append-only history requirement), so the
config governing that state needs to be anchored the same way — a config file
discovered by walking up the directory tree would be inconsistent with cwd-anchored
state. Separately, teams running the same pipeline from multiple machines need a way to
share cache hits and history without standing up a separate database service — hence
the optimistic blob-sync design rather than requiring a hosted metadata store.

## 3. Guide-Level Explanation

### 3.1 CLI

```bash
barca get pipeline.py --env staging
```

### 3.2 Python API

Not a distinct Python surface — `--env` and `barca.toml` resolution happen entirely in
the Rust binary that `barca.api` shells out to (see
[RFC-0002](/rfcs/0002-cli-surface/)).

Equivalent forms:

```bash
barca get pipeline.py --env staging        # CLI flag
BARCA_ENV=staging barca get pipeline.py    # env var
# or default_env = "staging" in barca.toml
```

```toml
# barca.toml
default_env = "dev"

[remote]
uri = "abfss://cont@acct.dfs.core.windows.net/barca/my-project"
state = "optimistic"       # "off" disables shared metadata (artifacts still remote)
push_retries = 5

[remote.storage_options.abfs]
account_name = "acct"
```

## 4. Reference-Level Explanation

### 4.1 Public API Surface

**Precedence** (every value, no exceptions): CLI flag > env var > `barca.toml` > default.

**`barca.toml` schema:** `default_env`, `[remote].uri` (enables remote mode),
`[remote].artifacts_uri` / `[remote].state_uri` (literal overrides, no `{env}`
templating), `[remote].state` (`"optimistic"` | `"off"`), `[remote].push_retries`,
`[remote.storage_options.<protocol>]` (forwarded verbatim to
`fsspec.filesystem(protocol, ...)`). **Unknown keys are hard errors** — typo protection
— as is a malformed file.

**Environment variables:** `BARCA_ENV`, `BARCA_REMOTE_URI`, `BARCA_ARTIFACT_URI`
(0.4.0-compat: literal, artifacts-only, bypasses env prefixing — warns if combined with
a non-default `--env`), `BARCA_STATE_URI`, `BARCA_STATE`, `BARCA_PUSH_RETRIES`,
`BARCA_STORAGE_OPTIONS` (JSON keyed by protocol, merged **over** the toml tables
per-key).

**Environment separation (`--env`):** names match `[A-Za-z0-9._-]+`. `default` env keeps
the pre-0.5.0 local layout (no migration needed for existing projects):

| | env = `default` | named env `<e>` |
|---|---|---|
| local DB | `.barca/metadata.db` | `.barca/envs/<e>/metadata.db` |
| local artifacts | `.barca/artifacts/` | `.barca/envs/<e>/artifacts/` |
| remote artifacts | `{uri}/default/artifacts/` | `{uri}/<e>/artifacts/` |
| remote state | `{uri}/default/state/metadata.db` | `{uri}/<e>/state/metadata.db` |

**Shared-state sync protocol (`state = "optimistic"`, the default once a `uri`
resolves):** pull the metadata DB blob at run start, run locally, then push back with an
etag/generation-conditional upload at run end. If another machine pushed first, re-pull
and replay this run's rows onto the newer base (bounded by `push_retries`) — nothing is
lost, no rows are silently dropped. Before upload, the WAL is checkpointed into the main
file, so the blob is always a complete, standalone SQLite file openable with stock
`sqlite3`. `state = "off"` shares artifacts but keeps metadata local — this is the
**required** setting for `barca serve` today (see
[RFC-0004](/rfcs/0004-http-server-api/) §4.5).

**What a pull does to the local database.** A pull happens at the start of `barca get` and
`barca run`, before `--dry-run` and `barca status` look, and on every push conflict. After
it, the local database holds exactly: every row of the pulled blob, plus the *unpushed*
local rows, each row once. Nothing else of the old local database survives, in particular
not its write-ahead log.

A local row is *unpushed* when the pulled blob does not have it, decided from the two
databases alone (there is no "pushed" marker to keep in step):

| row | identity | unpushed when |
|---|---|---|
| run (`runs`) | `run_id` | the blob has no run with that `run_id` |
| step (`materializations`) | `(run_id, node_id)`: a step has one outcome per run | the blob has no step with that pair |
| captured output (`logs`) | `run_id` | the blob has no line for that run |

This covers a run killed before its push (its run row and the steps it recorded as they
finished), a run whose push failed, and runs made with `state = "off"`. A run the blob
holds as `running` or `interrupted` while the local database holds the outcome the run
recorded for itself (`success`, `failed`, `cancelled`) takes the local outcome; in every
other case the blob's run row stands. Only runs the blob does not hold with such an
outcome are compared step by step: a run writes nothing after the push that carries its
outcome.

Not carried, by decision: a step row with no `run_id` (written before 0.17, which did not
record it, so it cannot be told from a pushed row); a successful step whose artifact is
not reachable from this machine (a local path that does not exist; a store URI is not
checked), because it would be a cache hit with nothing behind it; cost estimates and
scheduler state, which are not history.

Carried rows are local until the next push from that machine (the end of its next
`get`/`run`); read-only commands carry them and push nothing. A killed run therefore
resumes on its own machine at once and reaches other machines, as `interrupted`, with
that next push.

The sequence, all of it under the database's cross-process lock except the download:

1. Download the blob to `<db>.pull-<pid>-<n>` next to the database.
2. Carry: copy the unpushed rows from the local database onto the downloaded file, in one
   transaction. The local database is only read.
3. Fold: checkpoint both write-ahead logs into their main files and verify they are
   empty. Each database is now one self-contained file.
4. Swap: remove the local sidecar files (empty by now), fsync the downloaded file when it
   carries rows that exist nowhere else, and rename it over the local database.

A process killed before the rename leaves the old local database whole, unpushed rows
included; one killed after it leaves the new one whole. The next pull starts again from
step 1, and because step 2 adds only rows the target lacks, repeating it adds nothing
twice. A downloaded file that cannot be opened as a database fails the command (exit 3)
and leaves an existing local database untouched. A local database that cannot be opened
has nothing that can be carried: it is replaced, with a warning.

Because a pull keeps unpushed rows, it needs no knowledge of whether a run is live in the
project: a second `get`/`run`, `--dry-run` and `barca status` pull while a run is going,
and that run's row and recorded steps are still there afterwards. The pid and host on a
run row have one purpose, reporting a run whose process is gone as `interrupted`.

With `state = "off"` nothing is pulled or pushed, and none of the above runs.

### 4.2 Implementation Details

Resolution lives in `crates/barca-core/src/config.rs`; the shared-state pull/checkpoint/push
sequence lives in `crates/barca-core/src/state_sync.rs` (Rust side; the swap of the local
database is `db::replace_db`, and what it carries over is `state_carry.rs`) and
`python/barca/_state.py` (`python -m barca._state`, the Python-side counterpart for
backends gcsfs can't express a generation precondition on — see
[Remote Storage](/reference/remote-storage/) §Credentials).

### 4.3 Rust ↔ Python Boundary

Config resolution itself is Rust-only and precedes any worker spawn — workers never
re-resolve `barca.toml` independently; they receive already-resolved
storage/state parameters from the coordinator. The one place Python performs its own
credential resolution is per-backend fsspec construction during artifact I/O (each
backend's native default credential chain, see
[Remote Storage](/reference/remote-storage/) §Credentials) — `BARCA_STORAGE_OPTIONS`
values are what Rust hands to Python's `fsspec.filesystem(...)` call, unmodified.

### 4.4 Node-Kind Semantics

Not applicable — configuration is orthogonal to node kind/freshness.

### 4.5 Edge Cases

- `barca serve` refuses to start if config resolves to `state = "optimistic"` with a
  state URI — see [RFC-0004](/rfcs/0004-http-server-api/) §4.5.
- The state blob must stay under the object store's single-request upload limit (48 MiB
  for the S3-compatible backends, R2 included) — the coordinator errors clearly if it
  grows past that rather than silently truncating.
- `BARCA_ARTIFACT_URI` is a distinct, older (0.4.0-era) mechanism from `[remote].uri` —
  it moves *only* artifacts, leaving metadata local, and bypasses `--env` path
  prefixing (with a warning if a non-default env is also active). It is not a shorthand
  for `[remote].artifacts_uri`; the two can produce different paths.

## 5. Determinism, Caching & Testing

Environment separation (§4.1) guarantees dev/staging/prod never share cache or
artifacts, which is load-bearing for reproducibility across environments — a cache hit
in `staging` must never silently reuse a `prod` artifact. The optimistic
pull/replay-on-conflict protocol is what makes cross-machine cache hits safe
(see [RFC-0005](/rfcs/0005-artifact-serialization-and-storage/) §5): a machine that
loses the conditional-upload race never overwrites another machine's newer state, it
replays on top of it. Covered by the backend conformance suite (conditional create,
cross-machine cache hit, concurrent-writer conflict → replay, a pull over unpushed
local runs) run against MinIO / fake-gcs-server / Azurite on every PR, plus
`crates/barca-core/src/config.rs` unit tests for precedence resolution. The pull rules of
§4.1 are pinned by `crates/barca-core/src/state_carry.rs` (what is unpushed), the
`replace_db` tests in `crates/barca-core/src/db.rs` (the sequence, a death at each point,
unreadable and older-schema databases) and `python/tests/test_state_pull.py` (killed
runs, concurrent runs and read-only commands, across project directories sharing a state).

## 6. Performance

Config resolution is a fixed, small cost paid once per invocation (file read + parse +
precedence merge) — not itself benchmark-sensitive. The optimistic state pull/push
*is* on the hot path for remote-mode runs (network round-trip per run) — no dedicated
`benchmarks/` scenario exists yet for remote-mode overhead specifically (local-mode
`benchmarks/trivial` is unaffected, since remote mode is opt-in via `[remote].uri`).

## 7. Drawbacks

Two distinct artifact-relocation mechanisms (`BARCA_ARTIFACT_URI` 0.4.0-compat vs.
`[remote].uri`/`[remote].artifacts_uri`) is genuine surface-area debt — a user reading
only the newer docs could reasonably not know the older env var still exists and
interacts with `--env` differently.

## 8. Rationale & Alternatives

A required hosted metadata service (rejected) — e.g. a shared Postgres — was rejected
in favor of blob pull/push because it would add an operational dependency barca's
single-binary, zero-infrastructure design principle explicitly avoids. The
optimistic-with-replay conflict strategy (rejected alternative: pessimistic
locking/leases on the state blob) avoids a distributed-lock design entirely — conflicts
are rare in practice (most teams don't run the exact same pipeline from two machines in
the same instant) and replay-on-conflict degrades gracefully rather than blocking.

## 9. Prior Art

Dagster's code-location/deployment config and Prefect's workspace/profile model both
assume a hosted control plane; barca's blob-sync model has no direct equivalent in
either — see [Framework Comparison](/comparisons/framework-comparison/).

## 10. Unresolved Questions

Should `BARCA_ARTIFACT_URI` (0.4.0-compat) be formally deprecated now that
`[remote].artifacts_uri` covers the same need with consistent `--env` interaction?

## 11. Future Possibilities

Shared-state support in `barca serve` (currently a hard refusal, per
[RFC-0004](/rfcs/0004-http-server-api/) §4.5) is the most-requested gap this RFC's
model leaves open — it would need a locking or serialization story compatible with
axum's concurrent request handling, not just the CLI's one-run-at-a-time pull/push.
