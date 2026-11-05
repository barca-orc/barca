# Barca glossary

Two vocabularies. The first is what the CLI, the manual and the UI say to a user, owned by the
manual (`barca docs status`, "Cache state", for the states and reasons). The second is the
language of the specifications in `specs/`: a term describes the product concept; its storage
representation and implementation belong in the specifications.

## User vocabulary

One line each, consistent with `barca docs status`, `barca docs cache` and `barca docs remote`.

**cached**: a successful result matches this code and these inputs; `get` serves it without running the step.

**stale**: the node ran before, but `get` would run it again (its run hash moved, its last attempt failed, or its artifact is gone and something needs to read it).

**unsynced**: cached, but not where it should be: with a store, this machine's copy, the store's object and the recorded digest disagree (`local_only`, `store_only`, `mismatch`); with a server, the result is here but not yet in the server's ledger (`not_in_ledger`). The next `get` repairs it without running a step.

**blocked**: a partitioned asset whose missing keys wait on the same keys of an input, which failed; a run computes its other keys and skips these.

**never_run**: no successful materialization is recorded for the node.

**partial**: a partitioned asset with some keys cached and some missing.

**always_runs**: a task or sensor; never cached.

**unknown**: not decidable before something upstream runs: dynamic partitions whose source has not run, a sensor with no recorded output, an early-cutoff asset that would run first, or a node the dry run has no step for.

**run hash**: the cache key of one step: a digest of its code, the project code it calls, its decorator arguments that count, its inputs' run hashes, its sensors' values and its declared environment variables. Same run hash, same result.

**ledger**: the record of which results exist (run hashes, artifact paths, run history, schedules); the local metadata database, or the server's database in server mode, where the server is its only writer.

**store**: a remote object store (S3, GCS, Azure, R2, or a directory) holding copies of this machine's artifacts and backups of its metadata database: a backup and read-through cache, not shared state.

**environment**: a named, fully separate cache, history and backup set (`--env <name>`, under `.barca/envs/<name>/`); `default` without one.

**cutoff**: `@asset(cutoff=True)`: what reads the asset depends on the bytes it wrote, so a rerun with unchanged output reruns nothing below it.

**node**: one decorated function (`@asset`, `@task`, `@sensor`) identified as `file.py:name` relative to the project root.

**step**: one node in one run: the unit the plan schedules and the result reports (`ran`, `cached`, `skipped`, `failed`).

**attempt**: one execution of a step; a step retried (`retries=`) has several attempts, the last of which is its result.

**batch, call**: with `batch=`, one call of a partitioned step's function computes several partition keys; a failed batch is split into smaller calls, down to single keys.

## Specification vocabulary

**Asset**:
A reusable data result whose validity depends on its definition and inputs.
_Avoid_: cached task

**Task**:
An action requested for execution. Separate explicit invocations remain separate actions.
_Avoid_: uncached asset

**Sensor**:
An observation function that reports whether external state changed and may supply data.
_Avoid_: scheduled asset

**Partition key**:
The named coordinates identifying one part of a node's work or results.

**Partition domain**:
The current set of partition keys belonging to a node.
_Avoid_: partition history

**Observation**:
The external state or change signal reported by a sensor, together with any supplied data.

**Observation revision**:
The identity of an observation used to distinguish freshness changes from changes in data bytes.
An ordered observation revision additionally identifies which observation is newer.
_Avoid_: content hash, completion timestamp

**Run**:
An execution of a request or scheduled reconciliation, with its own progress and outcome.
_Avoid_: computation

**Computation**:
The work producing one asset result for a particular definition, inputs and partition key.
Several runs can need the same computation.
_Avoid_: run

**Materialization**:
A recorded production of a result. History can contain several materializations for the same key.
_Avoid_: current selection

**Current selection**:
The result selected for a node and partition under the inputs being considered; it can be an
older materialization reused from cache.
_Avoid_: latest materialization

**Freshness evidence**:
The information establishing whether a selection matches the definitions, membership and
observations against which it was validated.
_Avoid_: artifact availability

**Artifact availability**:
Whether a result's bytes are present locally, can be fetched, or are missing.
_Avoid_: freshness

**Reconciliation**:
Bringing requested assets and their dependency chains up to date with the relevant observations.
_Avoid_: unconditional refresh

**Subscription**:
A run's interest in the outcome of a shared computation.
