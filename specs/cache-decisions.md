# Cache decisions and run records

Specification version: 2. Status: implemented. This boundary covers what a cache decision may
rely on (complete upstream hashes, conservative dependency provenance), the order in which a
run's results become durable, and when a run without a recorded outcome is reported as
`interrupted`. The user-facing description is `barca docs cache`
([cache.md](../crates/barca-cli/docs/cache.md)); the hash inputs themselves and which decorator
arguments count are listed there and checked against `definition.rs`. Upload receipts are
[artifact-upload-receipts.md](artifact-upload-receipts.md); metadata compatibility is
[metadata-schema.md](metadata-schema.md).

Conformance: cache decision tests in [cache.rs](../crates/barca-core/src/cache.rs)
(`partial_upstream_cannot_hit_a_persisted_partial_cache_row`,
`complete_decisions_retain_hashes_across_worker_chunks`,
`aligned_key_present_does_not_allow_another_expanded_chunk_to_be_missing`),
[conditional bindings](../python/tests/test_conditional_helper_hashing.py),
[helper star imports](../python/tests/test_helper_star_hashing.py),
[pipeline star imports and binding proof](../python/tests/test_static_barca_bindings.py),
[unproven decorators](../python/tests/test_decorator_names_not_barcas.py),
[progress recording](../python/tests/test_incremental_persist.py),
[progress counts](../python/tests/test_progress_counts.py),
[terminal outcomes](../python/tests/test_terminal_persist.py),
early cutoff in [cache.rs](../crates/barca-core/src/cache.rs) (`cutoff_tests`),
[cone.rs](../crates/barca-core/src/cone.rs)
(`a_cutoff_asset_reached_by_name_adds_a_marker_not_its_code`) and
[test_asset_cutoff.py](../python/tests/test_asset_cutoff.py),
persistence tests in [persist.rs](../crates/barca-core/src/persist.rs),
run owners in [run_owner.rs](../crates/barca-core/src/run_owner.rs) and
[reconciliation](../python/tests/test_history_reconcile.py).

## Validated freshness before expansion

Default cache-aware execution may omit proven-fresh work before per-key expansion. Proofs
are private, rebuildable metadata, scoped to each requested logical node and artifact layout.
They contain the node's structural signature (definition, bindings, freshness, sinks,
partition specs, declared environment and upstream signatures), its validated key count,
and source observations or an unpartitioned output reference. Cutoff and Manual upstreams
retain their output-based freshness semantics. Tasks and sensors still use normal decisions;
Manual assets and compound partition nodes validate through the normal cache engine.

A proof is usable only at the current materialization generation (maximum row id). A
successful run can publish updated proofs only when no other run has materialized work
since its opening generation. Failed, interrupted and cancelled runs publish no proof.
Explicit refresh, partition selection and local execution using a remote server ledger
use normal decisions; their writes invalidate old generations. Missing or malformed proof
payloads fall back to validation. Database failures remain infrastructure errors.

Only changed same-dimension source keys propagate into aligned partition consumers.
Inherited membership edges do not turn a keyed value change into whole-source invalidation;
explicit whole-dictionary inputs do. Key additions/removals update membership and dataset
sink pruning. Fresh keys are represented by counts; artifact references and recovery
steps are loaded only when active inputs or returned values require them. A cached status
therefore does not assert the existence of an unused artifact. Missing needed artifacts
still recover through the existing dependency repair path. Cache eviction after a command
protects current results of proven-fresh nodes in its cone without loading each key.

Source discovery/comparison can be linear in source keys; changed fan-ins necessarily load
all collected inputs. No constant-time claim applies to those operations. The normal complete
hash requirement below applies to all affected work, including lazily loaded fresh inputs.

Conformance: [incremental DAG regressions](../python/tests/test_incremental_dag.py),
[missing artifact recovery](../python/tests/test_missing_artifact.py), dataset sink,
cutoff, telemetry and startup suites; [benchmark](../benchmarks/incremental_dag/bench.py).

## Complete upstream hashes

Before deciding a consumer, the coordinator checks that every upstream step in the expanded
plan has a run hash, including every upstream partition key across worker chunks. An
incomplete hash map fails as an infrastructure error (exit 3) before any consumer hash is
computed or any cache row is looked up: a result is never reused on a hash computed from a
partial set of upstream keys. The expected keys come from the selected expanded plan. Worker
pool size does not change valid hashes or cache decisions.

## Conservative provenance

Where a name's definition cannot be selected statically, the consumer hashes more, never less.
An unrelated edit may cause an extra recompute; an edit that can change the result always
changes the hash.

- **Conditional bindings.** A name bound inside a module-level `if`, `try`, `for`, `while`,
  `with` or `match` block is not resolved by evaluating the condition. A consumer reading it
  hashes the module source and follows its statically visible imports and references.
- **Unpacking and augmented assignment.** Module-level tuple, list or starred unpacking and
  augmented name assignments are treated the same way.
- **Retention.** This provenance is retained through later assignments, functions, classes and
  imports, because an earlier alias may have captured the uncertain value before a replacement.
  An unconditional replacement therefore does not clear it. Definitions that do not reach such
  bindings keep their selective cones.
- **Star imports in helper modules.** Consumers of functions, classes, captured constants and
  forwarded exports include the contributing module sources and their static dependencies,
  across multiple and transitive star imports. Conditional star imports are included without
  selecting a branch. This does not extend decorator discovery beyond its literal-export rules.
- **Star imports in a pipeline file.** A decorated function in a file with a top-level project
  star import hashes the imported module and its static imports; this holds when discovery
  proves the module's literal `__all__`.
- **Unproven decorators.** A decorator not positively bound to a barca import counts in full,
  and a node carrying one counts its whole module source and every dependency of the static
  cone. A walrus assignment to a decorator name anywhere in the module, including a local
  scope, makes that binding uncertain. An explicit reference to `globals`, `locals`, `vars`,
  `exec`, `eval` or `__builtins__` makes every imported barca name uncertain, including aliases
  and qualified or shadowed references. These checks cover the explicit syntax, not arbitrary
  reflective indirection or runtime side effects.

Installed packages, the standard library and dynamic imports are outside the tracking boundary.

## Early cutoff

An `@asset(cutoff=True)` (#628, experimental) changes what its consumers fold in, not its own
run hash (`cutoff` does not count in its definition):

- A consumer folds in, for each step of the cutoff asset it reads, `cutoff<partition>:<digest>`:
  the SHA-256 of the artifact bytes recorded for that step, and the partition it came from
  (empty when unpartitioned). Not the cutoff asset's run hash. A step whose digest is not known
  (a row recorded without one) contributes its run hash, as without `cutoff`.
- A consumer is decided only once the digest of what it reads is known: the cutoff asset ends
  its chain, its consumers are in a later phase, and they are not in the set asked of a server
  ledger before the run starts. A dry run reports a consumer of a cutoff asset that would run,
  and everything below it, `unknown` (`cutoff_pending`); it never predicts a hit or a miss for it.
- A `--refresh` cascade does not pass a cutoff asset, and a consumer cached through one carries
  no `--no-cascade` warning: its hash already holds the bytes the refresh wrote.
- A dict `partitions_from` source with `cutoff` contributes each key's value hash with the
  constant `cutoff` in place of its run hash.
- In the dependency cone, a cutoff asset reached by name (a consumer's parameter named after
  it) contributes a constant marker, not its definition or what it uses. This is the one place
  a cone hashes less than the conservative rule above: the asset's code reaches its consumers
  only through its bytes. Calling a cutoff asset as a plain function from another step is outside
  the tracking boundary.

## Recording progress

- A completed local result is queued for recording after its artifact is completely written.
  With a remote artifact store it is queued only after its upload is confirmed; an unfinished
  upload is never recorded as reusable progress.
- One recorder owns mid-run writes and commits queued rows at most twice a second. A batch
  commits its successful step rows and the running run's progress count in one transaction;
  replaying a batch does not duplicate rows or count a step twice.
- A failed batch is retained and retried at the same half-second interval, even when no further
  step finishes. Progress stays unrecorded while the database cannot commit.
- The final run write includes completed results the recorder has not saved. The terminal
  status and the successful and failed step rows commit together. If a required write fails,
  the command reports an infrastructure error (exit 3), keeps earlier committed progress,
  history and the previous run state (normally unfinished), and records no terminal outcome
  for a partially written ledger. Captured output is written separately, after the outcome
  transaction.

## Interrupted runs

A run is reported `interrupted` only on an observation that can be made only when its process
is gone; otherwise it stays `running`. A run records the kernel its process ran on (the boot
id), its pid namespace, its start time, and a marker: a FIFO in `.barca/run-owners/` that the
process holds open while it has a run in flight. Three observations count:

1. On the same kernel and in the same pid namespace, the process table: no process with that
   id, or one with another start time in the same known time namespace.
2. On the same kernel in another pid namespace (another container), the marker: the system
   refuses to open a FIFO for writing when no process has it open for reading, and it closes
   the owner's end when the owner ends, however it ended. This counts only where barca can show
   that the file is the one the owner held: same device and inode, and on Linux the same file
   handle.
3. On another kernel, that the owner's kernel has shut down: the marker is on a local disk (not
   a network or VM-shared mount) for both the owner and the reader, it is the same filesystem
   and inode, and nobody has it open. A local disk is mounted by one kernel at a time.

Machine ids, host names and clocks decide nothing. `barca history --reconcile RUN_ID` records a
single uncertain row as `interrupted` on the operator's assertion, and refuses a proven live
owner.
