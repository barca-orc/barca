# A cache-only get

Status: unapproved, unimplemented. Two things were decided on 2026-11-20 in the discussion of
[#1085](https://github.com/barca-orc/barca/issues/1085) and are the premise of this document:
the three tiers (disk, then the store, then a failure), and the latest success as the result.
Everything else here (the names, the wire shapes and the answers under "Open questions") is a
draft to decide. Nothing in it is a current contract; the owners it would change are the
[CLI contract](../../crates/barca-cli/docs/contract.md), the HTTP API,
[execution](../behavior/execution.md) and [resources](../behavior/resources.md).

## The problem

A service wants the last result of an asset that barca already has, without a read becoming a
run. `barca get` reuses unchanged assets but always executes sensors, and a changed sensor
value recomputes what is downstream of it (reproduced on 0.46.0 in #1085). `--dry-run` runs
nothing and returns no value. `barca sql` reads results but is not addressed by asset and has
no "there is no result" outcome a caller can branch on.

## Proposed behavior

`barca get <target> --cached-only` and `barca.get(target, cached_only=True)` answer from what
is recorded and execute nothing: no sensor, asset or task runs, no run is recorded or queued,
and no source file is imported.

### The lookup

For the target, and for each selected partition key on its own:

1. **The result.** The latest successful materialization in the metadata database of the
   selected environment. A failed run and a run in progress never replace it. No such row: a
   miss, reason `never_run`, and no tier is tried.
2. **Tier 1, disk.** The file the row names exists on this machine: it is the answer. No
   network call is made.
3. **Tier 2, the store.** The file is gone (evicted) and the row has a store copy: it is
   fetched into the local cache, checked against the recorded hash, and is the answer. This is
   the fetch a run already performs for an evicted cache hit.
4. **Tier 3, a failure.** Otherwise a miss, reason `evicted` (no store copy is recorded or no
   store is configured) or `not_fetched` (a store copy exists and the caller excluded tier 2).

### Rules

- **Stale results are answers.** The latest success is returned even when changed code or
  inputs would make an ordinary run recompute it. The answer says which it is (`fresh`).
  Deciding `fresh` reads source statically, as `barca status` does; it runs no sensor, so a
  sensor's current value is not part of it.
- **The caller chooses the tiers.** `--cached-only` is disk, then the store, then a miss.
  `--cached-only=local` is disk, then a miss: no store call of any kind, which is what #1085
  asks for.
- **A store failure is not a miss.** A store that is unreachable or refuses the credentials,
  and a fetched object whose bytes do not match the recorded hash, are errors (exit 3). A
  caller's fallback for "no result" must not fire because a credential expired.
- **No partial bytes.** A fetched file becomes visible at its path only when complete. A local
  file is returned only if the row names it and it exists; gc deletes a file before it records
  the eviction, so a lookup racing gc sees the file or a miss.
- **Partitions.** The selection is never widened. Each selected key goes through the tiers on
  its own, and the answer lists each key as a hit or a miss with its reason. The call fails if
  any selected key is a miss.
- **On a server.** The server performs the lookup against its own database, disk and store.
  A remote caller receives identity and format, and a signed URL when it asks for one
  (`-o url`); a path on the server's disk is reported as such and may not be readable by the
  caller.
- **Ordinary `get` is unchanged.**

### What a hit reports

Per result: node, partition key, run hash, format, size, the local path, `fresh`, and
`source` (`disk` or `store`).

### What a miss reports

Exit 1 with kind `cache_miss` in the stderr error envelope, naming each missing node or key
and its reason. In Python, `barca.CacheMiss`, a subclass of `BarcaError`, carrying the same
list.

## Not in the first version

- A content digest in the answer.
- A bounded binary-access interface for a remote caller reading a large table.
- A result that exists only in the store because another machine computed it (a server's
  ledger) and that this machine's database has no row for. Tier 2 covers what this database
  records.

## Open questions

- Whether a tier-2 fetch counts as a read for gc. Proposed: yes, it sets the last-read time as
  any read does, so repeated lookups do not fetch again; the fetched file counts against
  `[cache] max_size`.
- Whether a miss is exit 1 with a new kind (proposed) or a new exit code.
- Whether a partitioned target with some keys missing fails the call (proposed) or returns the
  keys it has with exit 0.
- Which release. It is new surface, so a minor; it depends on neither the run lifecycle
  (#944) nor resource bounds (#1054).

## Conformance, when implemented

1. With a result seeded, sensor state, code and inputs then changed: the previous result is
   returned with `fresh: false`, and no step executes, no run is recorded or queued.
2. Never run: a miss `never_run`, with no store call.
3. Evicted with a store copy: fetched and returned with `source: store`; with
   `--cached-only=local`, a miss `not_fetched` and no store call.
4. Evicted with no store copy: a miss `evicted`.
5. A store that refuses the credentials, and a fetched object with wrong bytes: exit 3, not a
   miss.
6. A failed run and a run in progress after a success: the success is still the answer.
7. A partition selection: only the selected keys are looked at, each reported on its own.
8. The CLI, the Python API and a server give the same answer for the same state.
