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

> **Amended (after 0.17.1, issue #221):** §4.1 now defines what a pull does to the local
> database, in "What a pull does to the local database". A pull used to replace only the
> main database file and leave the old write-ahead log beside it, which could drop other
> machines' history at the next push; and since 0.17.0 a run records its finished steps
> locally before its push. "Nothing is lost" now also covers local rows that were never
> pushed, a killed run's included, and a download that a concurrent pull or push has
> overtaken is never swapped in.

> **Amended (0.18, issue #243):** §4.1 now states what a downloaded blob must be before it
> may replace the local database ("A valid blob"), and that the database a pull replaces is
> kept as `<db>.prev` ("The previous database"). Before, only a file without a SQLite header
> or with a ragged length was refused; an empty object, a blob cut at a page boundary or
> overwritten inside, or another program's database was swapped in. An invalid blob now
> fails the command (exit 3) with nothing changed, locally or in the store. Also, from the
> review of the #221 amendment: the locked third download of a pull is bounded (45 s), a push
> that finds local writes pushes once more rather than up to `push_retries` times, and the
> base record's sequence number wraps instead of sticking at its largest value.

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
it, the local database holds: every row of a blob that was the shared state when the pull
began or later (the one the returned token names), plus the *unpushed* local rows, each
row once. Nothing else of the old local database survives, in particular not its
write-ahead log. The local database is never replaced by a blob older than the one it is
based on.

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

A run present on both sides is compared step by step only when the blob holds it as
`running` or `interrupted` and its `(status, steps_executed)` differs from the local row
(a run's `steps_executed` moves with every step it records).

*Finding the unpushed rows does not read the history.* History is append-only: a run's
row keeps its row id, a pull appends the rows it carries after the last row of the
download, and a push uploads the whole file, so every blob is its predecessor plus
appended rows and a local database is a blob plus appended rows. Local runs are therefore
read from the newest backwards, each looked up in the blob by `run_id` (a unique index),
until one is found there in the same row: from that row back, both hold the same runs.
Unfinished runs are found through an index on `runs.status`. A blob that shares no
history with the local database (the shared state was reset) never matches, and every
local run is carried.

Not carried, by decision: a step row with no `run_id` (written before 0.17, which did not
record it, so it cannot be told from a pushed row); a successful step of a run made on
this host whose artifact is not reachable from this machine (a local path that does not
exist; a store URI is not checked), because it would be a cache hit with nothing behind
it (steps of other hosts' runs are never filtered: they came from the shared state); cost
estimates and scheduler state, which are not history.

**The base record.** A download takes time and holds no lock, so by the time a pull swaps
it in, another process may have pulled a newer blob or pushed. `<db>.base` is a local
file, never uploaded, written only under the database's cross-process lock: before every
swap, after every swap and after every push. It holds a sequence number, incremented on
every write (wrapping; started from the clock when there is no readable predecessor), a
digest of what the last pull carried, used only to print the `kept` line once per set of
rows, and the token of the blob the last swap put in place, used only to decide whether
`<db>.prev` is replaced (below).

A pull reads the file's bytes before downloading and again under the lock before
swapping. If they differ, the download is discarded and the pull starts again; the third
attempt holds the lock from before the first read until the swap is done, so it cannot be
overtaken. Other barca commands in the project wait for that lock (for up to 60 seconds),
so that one download is bounded: after 45 seconds it is stopped and the pull fails (exit
3) with the local database untouched. Nothing else is ever concluded from the file, in particular nothing about what
the local database contains: every pull downloads, carries and swaps. A missing,
unreadable or foreign file therefore cannot cause a loss (the two reads are equal, or
they differ and the pull downloads again), and a file left by a process that died
mid-swap is just another state.

**Push.** Under the lock the write-ahead log is folded in and the main file is copied to
`<db>.push-<host>-<pid>-<n>`; the lock is released and the copy is uploaded
conditionally. The upload therefore sends one consistent database and keeps no other
command waiting. Afterwards the lock is taken again and the base record is advanced. If
the record had changed (a pull replaced the database) or the write-ahead log is no
longer empty (something was written; every write goes there first), the push is treated
like a conflict, once: pull, replay this run's rows, push again. If the second upload
finds the same, the upload stands and the later rows stay local until the next push: a
run going in the same project writes during every upload, and it pushes its own rows when
it ends. (Conflicts with another machine's push are still retried up to `push_retries`
times.)

**A valid blob.** A downloaded blob replaces the local database only if it is valid.
Valid means all of the following, checked in this order on the downloaded file, before
anything is written to it:

1. It is a whole SQLite file: not empty, starts with the SQLite header, and its length is
   a whole number of pages. An empty object is invalid, not "no history yet" (that is the
   object being absent): a push uploads a checkpointed main file, which is never empty.
2. Every page of it can be read: it opens, its schema can be read, and `PRAGMA
   integrity_check` returns `ok`. This is the only check that finds a blob cut at a page
   boundary, pages overwritten inside it, or an index that disagrees with its table. It
   reads the whole file (about 6 ms per MB), and is skipped in exactly one case: the
   download is byte-for-byte the local database with its log folded in, where replacing
   one with the other changes nothing.
3. It is a barca history: it has both the `runs` and the `materializations` table. Every
   version that could push created both first, so a database without them was not written
   by barca; it is refused rather than turned into a history by creating the tables.
4. This version can use it. barca's additive migrations are applied to the download
   (never to the local database), and then each of barca's tables must have every column
   this version reads or writes, and no other column that this version's inserts could
   not fill (`NOT NULL` without a default). A blob from an older barca is valid, because
   the migrations complete it. A blob from a newer barca is valid exactly while this
   version can still read and write it: unknown tables and unknown nullable or defaulted
   columns are left alone, which is how machines on different versions share a history
   today. There is no schema version number to compare (#82); when one exists, comparing
   it belongs to this rule.

An invalid blob is an error of the shared state, not of the connection: the command exits
3 with a message whose first line names the object and says which rule failed (for an
integrity check: how many problems, and the first), followed by where the repair is
described and then the list of problems, and nothing is changed: not the local database, its log or its unpushed
rows, not `<db>.prev`, not the base record, not the blob. No command repairs or
overwrites an invalid blob on its own; the repair is manual (restore an earlier version
of the object, or remove it and run on the machine with the most complete local database,
which then creates it by the bootstrap rule; an upload is not validated, so that database
must be intact itself. Histories damaged by 0.17.1 and earlier, where the local copies are
damaged too, are salvaged with SQLite's `.recover` or started again: `barca docs remote`
has both procedures). A failure of the machine while checking
(disk full, a file that cannot be written) is reported as that, not as an invalid blob.

After the carry and the fold, the file about to be swapped in is checked again to be a
whole SQLite file. The full page check is not repeated: what was added is one committed
transaction and a checkpoint by the same engine that writes the local database on every
run, and repeating it would cost its full price on every pull during a live run.

**The previous database.** The local database a pull replaces is kept as `<db>.prev`:
one generation, one self-contained file (the log is folded in before the swap), exactly
what was replaced, unpushed rows included. It is kept by the swap rather than by a copy:
before the rename, `<db>.prev.tmp` is made a second name (hard link) for the local
database file; the rename then leaves the old file with that one name; after the rename
it is renamed to `<db>.prev`. Where the filesystem has no hard links the second name is a
copy, fsynced before the rename. `<db>.prev` therefore changes only by a rename and is
always one whole generation, and keeping it costs the same for any size of history. The
name is published after the swap, not before, because until the swap the two names are
one file: published earlier, a pull that then failed would leave `<db>.prev` following
every later write to the live database. A pull killed between the swap and that last
rename has replaced the database without updating `<db>.prev` (it holds the generation
before); the leftover `.prev.tmp` is removed by the next pull. If that last rename fails,
the swap stands and a warning on stderr says the replaced database was not kept.

`<db>.prev` is replaced only when the swap brings in something new. Not when there was no
barca history to replace (no local database, an empty file, a file that is not a
database). Not when the download is byte-for-byte the local database. And not when the
download is the same version of the blob as the last swap put in place (the token in the
base record): the local database is then that version plus unpushed rows, which are
carried again, and replacing `<db>.prev` would overwrite the generation from before that
version with a copy of what is already there. So `<db>.prev` is the local database as it
was before the last pull that brought in a different version of the shared state. barca
never reads it; restoring it is a manual copy over `<db>` with the `-wal` file removed.

**Reset and rollback.** Because unpushed means "absent from the pulled blob", removing
history takes more than changing the blob. If the blob is deleted, the next run on any
machine creates it again from its whole local database (the bootstrap rule). If it is
replaced by an older one, every machine brings back the runs it holds that the older one
lacks. Resetting on purpose means deleting the blob and, on each machine, `metadata.db`,
`metadata.db-wal` and `metadata.db.base`.

The sequence of every pull; all of it under the database's cross-process lock except the
download:

1. Read the base record, then download the blob to `<db>.pull-<host>-<pid>-<n>` next to
   the database.
2. Still current: re-read the base record; if it changed, discard the download and start
   again.
3. Valid: check the downloaded file ("A valid blob"); if it is not, fail with nothing
   changed.
4. Carry: copy the unpushed rows from the local database onto the downloaded file, in one
   transaction.
5. Fold: checkpoint both write-ahead logs into their main files and verify they are
   empty. Each database is now one self-contained file.
6. Swap: give the local database file its second name (`<db>.prev.tmp`), advance the base
   record, remove the local sidecar files (empty by now), fsync the downloaded file when
   it carries rows that exist nowhere else, rename it over the local database, rename
   `<db>.prev.tmp` to `<db>.prev`, and advance the base record again.

A process killed before the rename leaves the old local database whole, unpushed rows
included; one killed after it leaves the new one whole. The next pull starts again from
step 1, and because step 4 adds only rows the target lacks, repeating it adds nothing
twice.

Failures never replace a local database that might hold rows. A downloaded file that is
not valid fails the command (exit 3) and leaves the local database untouched, whether or
not there is one. A
local database that is held open by another program (after a wait of 5 seconds), cannot
be read, has an empty main file beside a non-empty log, or fails with any error not
recognised as corruption also fails the command (exit 3) untouched. Only a local file
that certainly holds no barca history is replaced, with a warning naming the reason: no
SQLite header, a length that is not a whole number of pages, the engine's not-a-database
or corrupt error, an empty file with no log, a database without barca's tables, or a log
whose main file is missing.

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
