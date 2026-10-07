---
title: Core Constraints
description: The rules barca was designed around, which of them 0.18.0 implements, and what happens where it does not.
---

These are the rules barca was designed around. Some were written before the code existed. Each
section gives the rule, whether 0.18.0 implements it, and what happens where it does not.

| Constraint | In 0.18.0 |
|---|---|
| [Graphs are acyclic](#directed-acyclic-graphs-only) | Implemented |
| [Python interpreter resolution](#python-interpreter-resolution) | Implemented (no `uv` requirement) |
| [Preflight consistency](#preflight-consistency) | Not needed between commands; not checked during a run |
| [History is append-only](#history-is-append-only) | Rows are kept; no status flags, no pruning command |
| [Freshness declarations](#freshness-declarations) | Only `Schedule` has an effect |
| [Caching follows provenance, not recency](#caching-follows-provenance-not-recency) | Implemented |
| [Asset identity](#asset-identity) | Implemented; no rename detection |
| [User code is what runs](#user-code-is-what-runs) | Implemented; no source snapshots |

## Directed acyclic graphs only

**Rule.** The graph has no cycles: no node depends on itself, directly or through other nodes.
Iteration belongs inside one function.

**Status.** Implemented. Every command that reads the project checks the graph before
anything runs:

```
$ barca list cyc.py
{"code":2,"error":"DAG error: cycle detected in dependency graph","kind":"usage","remediation":"Fix the inputs between definitions, then run `barca list cyc.py` to check each node's inputs."}
```

The exit code is 2. The same check rejects an input that names an unknown node, a sensor with
inputs, and a task used as an input to an asset or a sensor.

## Python interpreter resolution

**Rule.** Steps run in the Python environment barca is installed in.

**Status.** Implemented. Barca uses the `python` (or `python3`) that sits beside the `barca`
executable, which is the virtualenv barca was installed into. If there is none, it uses
`python3` from `PATH`. An early version of this page proposed requiring `uv`. That was never
built: barca does not need `uv`, does not check for it and does not manage an environment.
Python 3.12 or later is required.

## Preflight consistency

**Rule (original).** Before running a planned step, check that the function on disk still has
the definition hash the plan was made with, and fail if it does not.

**Status.** The original rule assumed a stored plan that a later command executes. Barca has
none: `barca get`, `barca run` and every run started by `barca serve` parse the source and
plan again, so a plan cannot be stale between commands.

Within a run there is no check. The source is hashed when the run is planned, and the worker
imports the file when the step executes. If you edit a function while a run that uses it is
going, the step may run the new code and be recorded under the hash of the old code. Do not
edit pipeline files during a run; if you did, run the affected assets again with
`--refresh <asset>`.

## History is append-only

**Rule.** Barca does not delete results or history as part of normal operation.

**Status.** Partly implemented.

- Every step result adds a row to the `materializations` table and every run adds a row to
  `runs`. No barca command deletes rows.
- Artifact files are kept. When a function's code or inputs change, the new result is written
  to a new file (`.barca/artifacts/{node}/{run_hash}{ext}`) and the old file stays.
- A result can be overwritten in place: `--refresh` runs a step again under the same run
  hash, and a function that is not deterministic then writes different bytes to the same
  path.
- The original design also called for a record of each definition and for status flags
  (stale, superseded, inactive). Those do not exist. A node that is removed from the source is
  no longer listed; its rows and files stay on disk.

### Pruning

There is no `prune` or `gc` command in 0.18.0, and `.barca/artifacts/` has no size cap. To
reclaim space, delete files under `.barca/artifacts/` yourself. Barca handles a missing file:
when a step needs to read a cached result whose file is gone, the step that produced it runs
again (the JSON output gives the reason `artifact_missing`). A missing file that nothing needs
to read costs nothing. Deleting all of `.barca/` removes the cache and the history.

## Freshness declarations

**Rule.** Every asset, sensor and task declares how it is kept up to date with `freshness=`:
`Always` (the default for assets and tasks), `Manual` (the default for sensors) or
`Schedule("<cron>")`.

**Status.** Only `Schedule` has an effect at run time.

- `Schedule("<cron>")` makes `barca serve` run the node on each cron tick. See
  [Scheduling](/scheduling/).
- `Always` and `Manual` are parsed, recorded and shown by `barca list` and in the plan JSON.
  Nothing acts on them. A `Manual` asset is computed by `barca get` like any other asset, a
  `Manual` upstream does not hold back anything downstream, and `barca serve` does not run an
  `Always` node on its own. Barca accepts `Always` on a sensor.

What `Always` and `Manual` should do in `barca serve` is proposed in RFC-0008
([PR #276](https://github.com/barca-orc/barca/pull/276)). Until then, whether a function runs
is decided by the cache alone (next section), and by `--refresh`.

## Caching follows provenance, not recency

**Rule.** A result is valid if it was computed from the current code and the current inputs,
however long ago. The most recent result is not special.

**Status.** Implemented. A result is stored under its run hash, which covers the function's
code, the helper code it reaches, and its inputs. If the code changes from version A to
version B and back to A, the results computed for A are cache hits again:

```
$ barca get b prov.py --json        # a returns 1
... "final_output":2 ... "steps_executed":2
$ # edit a to return 5
$ barca get b prov.py --json
... "final_output":6 ... "steps_executed":2
$ # edit a back to return 1
$ barca get b prov.py --json
... "final_output":2 ... "id":"prov.py:a" ... "status":"cached" ... "id":"prov.py:b" ... "status":"cached" ... "steps_executed":0
```

This is why old artifact files are kept. What the hash does not see is listed in
`barca docs cache`.

## Asset identity

**Rule.** A node's identity is its explicit `name=` if it has one, otherwise the file path
relative to the project root plus the function name (`pipeline.py:orders`). Two nodes may not
share an identity.

**Status.** Implemented.

```
$ barca list dup.py
{"code":2,"error":"DAG error: duplicate continuity key: 'same' defined in both 'dup.py' and 'dup.py'", ...}
```

- A node with `name=` keeps its cache and history when its file is moved or its function is
  renamed.
- A node without `name=` that is moved or renamed gets a new id. It is computed again, and the
  results and history under the old id stay on disk under the old id.
- The original design mentioned suggesting a probable rename from source similarity. That
  does not exist.

## User code is what runs

**Rule.** The worker imports your module from your project and calls your function, so
imports, tracebacks and helper modules behave as they do outside barca.

**Status.** Implemented. The worker loads the pipeline file from source with whichever Python
was resolved above. An early version of this page said barca also stores a snapshot of each
function's source for provenance. It does not: the metadata database holds hashes, not source.

## What follows from these

- Only `.py` files that import barca are read. Notebooks are not.
- The graph is validated on every command.
- Helper code a function reaches is part of its hash, so changing a helper invalidates the
  functions that use it.
- Old results stay usable, and stay on disk, until you delete them.
