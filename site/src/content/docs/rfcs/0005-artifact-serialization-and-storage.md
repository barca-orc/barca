---
title: 'RFC-0005: Artifact Serialization & Content-Addressed Storage'
description: 'json/pickle/parquet artifact formats, local and remote storage, staged writes, and @sink.'
---

- **Status:** Accepted (retroactive baseline — documents behavior as of v0.6.1)
- **Date:** 2026-07-16
- **Touches:** barca-core | python/barca
- **Supersedes / Related:** [RFC-0006](/rfcs/0006-configuration-and-remote-state/) (remote URIs, shared metadata DB)

---

> **Amended (after 0.18.0, issue #246, decided):** artifact objects stay at
> `{node}/{run_hash}{ext}`. Addressing them by a hash of their bytes was proposed (#246)
> and will not be done.
>
> Immutability follows from purity. An asset is a pure function of its code and its inputs,
> so its run hash identifies its bytes, and the object at `{node}/{run_hash}{ext}` is
> immutable in practice. State from outside enters through sensors: a sensor always runs,
> and the hash of what it returned is folded into the run hash of every asset that reads it
> (`cache::compute_run_hash`), so a changed observation lands at a new path instead of
> changing an old one.
>
> The one case where bytes change under the same run hash is a recompute of an asset that
> is not deterministic (`--refresh`, or two machines computing the same step at once). The
> object is then overwritten in place: same path, other bytes. Checked on 0.18.0 plus #263
> with an asset that returns a random id: after `--refresh` on one machine the store holds
> one object for the node, with new bytes. A machine that shares the history pulls the new
> recorded hash, replaces its stale local copy and says nothing. A machine whose history
> still has the old hash (one that keeps its history local, or a refresh that was killed
> before it recorded) uses the store's bytes and is told: a `[barca] warning:` line on
> stderr, and `steps[].artifact_mismatch: true` in the JSON result
> ([CLI contract](/reference/cli-contract/)). "Content-addressed" in this RFC's title and
> text therefore means addressed by run hash, as §8 says.

## 1. Summary

Data never passes between worker processes in-memory — every asset/task output is
serialized to an artifact file (json, pickle, or parquet), and downstream steps read
their inputs back from disk. In remote mode, artifacts are written content-addressed
(`{uri}/{env}/artifacts/{node}/{run_hash}{ext}`) so a cache hit on one machine is valid
on every machine.

## 2. Motivation

Barca's worker model is stateless, short-lived processes with no shared memory (see
[Architecture Decisions](/architecture-decisions/) — Rust owns all persistence, Python
workers have no DB access). Passing data in-process isn't an option once execution
crosses a process boundary; the only question is the wire format, and that format
needed to (a) support arbitrary Python objects (pickle), (b) support human-inspectable
structured data (json), and (c) support efficient columnar data (parquet) — without
forcing every user into one format.

## 3. Guide-Level Explanation

### 3.3 Decorator surface

```python
from barca import asset, sink

@asset(serializer="parquet")            # explicit format
def prices() -> "pd.DataFrame": ...

@asset()
@sink('./output.json')
@sink('abfss://exports@acct.dfs.core.windows.net/output.parquet', serializer='parquet')
def banana() -> dict:
    return {'a': 1}
```

Format precedence for a sink: `serializer=` kwarg → path extension
(`.json`/`.pkl`/`.pickle`/`.parquet`) → the parent asset's own artifact format.

## 4. Reference-Level Explanation

### 4.1 Public API Surface

**Formats:** `json`, `pickle`, `parquet` (parquet requires the `parquet` extra —
pyarrow/pandas). Format is chosen per-asset (`serializer=` kwarg, defaulting by return
type) and independently per-`@sink`.

**Local layout:** `.barca/artifacts/{node}/...` (or `.barca/envs/{env}/artifacts/` under
a named `--env`, see [RFC-0006](/rfcs/0006-configuration-and-remote-state/)).
`materializations` rows in `.barca/metadata.db` key each artifact by
`(node_id, run_hash)` and record path/format/size/elapsed time.

**Remote content-addressing:** in remote mode, artifacts are written to
`{uri}/{env}/artifacts/{node}/{run_hash}{ext}` — immutable, since the path is derived
from the run hash. Any machine with the same `run_hash` gets a valid cache hit against
the same object, with no coordination needed beyond the shared metadata DB (see
[RFC-0006](/rfcs/0006-configuration-and-remote-state/)). **Local (non-remote) artifacts
are keyed by node id, not content** — re-runs overwrite the same path. This is an
intentional asymmetry: local mode has no cross-machine consistency problem to solve.

**`final_output` envelope** (what `barca get`/`POST /run` return, see
[RFC-0002](/rfcs/0002-cli-surface/)): `{ "path": "...", "format": "json"|"pickle"|"parquet",
"size_bytes": N }`. `barca.api`'s `_read_output()` deserializes this transparently — the
envelope is an implementation detail of the CLI/HTTP JSON contract, not something a
`barca.get()` caller needs to unwrap themselves.

**Partitioned sinks:** each partition writes its own file with the partition key
injected before the extension — `@sink('out.parquet')` on `ticker=AAPL` produces
`out_ticker_AAPL.parquet`.

### 4.2 Implementation Details

`python/barca/_artifacts.py` owns format detection and json/pickle/parquet I/O;
`python/barca/_storage.py` owns local vs. remote (fsspec) backend selection. Staged
writes (temp file → atomic rename/upload) prevent partial artifacts on crash — see
[Remote Storage](/reference/remote-storage/) for the full write/read sequence, which
this RFC treats as incidental (§4.2, not §4.1): the *fact* that writes are atomic is
load-bearing (a reader never sees a partial file); the *mechanism* (temp file location,
`os.replace` vs. chunked `put_file`) is free to change.

### 4.3 Rust ↔ Python Boundary

Serialization and deserialization happen entirely in Python workers — the Rust
coordinator never reads artifact *contents*, only the metadata (`path`/`format`/`size`)
recorded back over the worker protocol. Two documented exceptions where Rust *does*
read a local artifact file directly (both are v1 limitations, not the general model):
dynamic partitions (`partitions_from=...` reads the partition source artifact from
local disk — fails explicitly under a remote store) and `parallel()` return values
(child results are read back from local JSON artifacts to resume the parent — under a
remote store the parent gets `null` results with a warning). See
[Remote Storage](/reference/remote-storage/) §"v1 limitations."

### 4.4 Node-Kind Semantics

Sensors' `(update_detected, output)` tuple is serialized like any other output; tasks'
outputs are serialized but never cached (per [RFC-0001](/rfcs/0001-node-kinds-and-freshness/),
node kind gates caching, not serialization — a task's output still needs to be an
artifact file if another task consumes it).

### 4.5 Edge Cases

- Switching `BARCA_ARTIFACT_URI` between runs does not invalidate cache rows pointing
  at the previous store (artifacts-only mode is keyed by node id, not content — see
  §4.1).
- A run that pulls the shared metadata DB successfully but crashes mid-way uploads
  nothing; its local rows are discarded by the next pull and those steps recompute
  (ties into [RFC-0006](/rfcs/0006-configuration-and-remote-state/)'s shared-state
  contract).
- A cache row whose artifact is gone (#252). The row stays a cache hit; whether the
  artifact can be read is decided only when something needs to read it — a step that is
  going to run takes it as an input, a `partitions_from` step is expanded from it, or it is
  the output the command returns (its targets; with no target, the one asset whose value is
  `final_output`, per [RFC-0002](/rfcs/0002-cli-surface/); a partitioned returned asset
  is returned whole, so each of its partitions is checked). A needed artifact that is neither on this machine's disk nor
  fetchable from the artifact store has its producing step run again in the same run, with
  reason `artifact_missing`; the run hash is unchanged, so the artifact lands at the same
  path and no downstream row is invalidated. For a store-backed row the readable copy is
  the local mirror: while a mirror exists the recorded store location is not consulted, and
  the store is asked only when the mirror is absent. "Not fetchable" means the store
  answered that the object does not exist **and** the store itself is positively there: its
  bucket, container or root directory answered a listing (asked once per run, before the
  first recompute). A store that is gone, misnamed or unreachable, and any other fetch
  failure (permissions, timeout), fails the run with exit 3 with nothing recomputed,
  uploaded or created. An artifact nothing reads is never checked, so pruned intermediates
  and unreturned assets at the end of a pipeline are not recomputed. `--dry-run` and
  `barca status` apply the same rule to what is on disk, including a store that is a
  directory; they do not contact a remote store. Implemented in
  `crates/barca-core/src/recover.rs`.
- A `@sink` failure (missing extra, bad credentials, unreachable endpoint) never fails
  the parent asset — logged as `[barca] SINK FAILED: ...` and recorded in run metadata.

## 5. Determinism, Caching & Testing

Content-addressing (§4.1) is the direct mechanism behind the provenance-based freshness
invariant in [Core Constraints](/core-constraints/): a `run_hash` match means the exact
same artifact bytes would be produced, so the existing artifact is reused rather than
recomputed. SHA-256 is the hash function (`crates/barca-core/src/hash.rs`). Every
remote backend (S3, GCS, Azure ADLS Gen2, R2) is held to the same conformance suite —
conditional create, cross-machine cache hit, concurrent-writer conflict → replay — run
against local emulators (MinIO, fake-gcs-server, Azurite) on every PR. Local-mode
artifact I/O is covered by `python/tests/`; the conformance suite lives alongside the
remote-storage integration tests.

## 6. Performance

Serialization format choice materially affects per-step wall time for large payloads
(parquet vs. pickle vs. json) but is not part of barca's fixed orchestration overhead —
see `benchmarks/large_payloads` and `benchmarks/mixed_io_cpu` for the relevant
scenarios. Staged writes (temp file, then atomic commit) add a bounded, one-time I/O
cost per artifact regardless of size.

## 7. Drawbacks

Three formats (rather than one canonical format) means format-selection logic
(§4.1 precedence rules) is itself a small surface users need to reason about, and it's
possible to pick a format that doesn't round-trip a given Python object (e.g. json on a
non-JSON-serializable return value) with the failure only surfacing at materialization
time, not at parse time.

## 8. Rationale & Alternatives

A single fixed format (rejected — e.g. pickle-only) would be simplest but forces every
consumer to be Python and forces every user into pickle's cross-version fragility for
data that's naturally tabular. Content-addressing by `run_hash` (rejected alternative:
content-hash the artifact bytes themselves) was chosen because `run_hash` is already
computed for cache-lookup purposes — reusing it for the storage path means no second
hash pass over potentially large artifact data.

## 9. Prior Art

Dagster's IOManager abstraction and Prefect's result storage serve the same purpose
with a more pluggable (and more configuration-heavy) interface; see
[Framework Comparison](/comparisons/framework-comparison/).

## 10. Unresolved Questions

Should dynamic-partition source reads and `parallel()` return values (§4.3) grow remote
support, closing the two documented v1 local-disk-only exceptions?

## 11. Future Possibilities

A content-hash (rather than node-id-keyed) path for local-mode artifacts would remove
the local/remote asymmetry noted in §4.1, at the cost of local disk usage no longer
mapping 1:1 to "current" outputs (ties into the `barca prune` command described as
future work in [RFC-0002](/rfcs/0002-cli-surface/) §11).

The same idea was considered for remote objects (a path that includes a hash of the bytes,
#246) and declined: for pure assets the run hash already identifies the bytes, and the one
exception, a recompute of an asset that is not deterministic, is reported as a hash mismatch
rather than prevented (see the amendment at the top).
