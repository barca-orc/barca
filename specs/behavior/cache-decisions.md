# Cache decisions and run records

## Scope and status

Specification version: 7. Status: implemented; the concurrent-version ordering gap is tracked in [freshness](freshness.md).
Scope: what a cache decision may rely on (complete upstream hashes, conservative dependency provenance), the order in which a
run's results become durable, and when a run without a recorded outcome is reported as
`interrupted`. The user-facing description is `barca docs cache`
([cache.md](../../crates/barca-cli/docs/cache.md)); the hash inputs themselves and which decorator
arguments count are listed there and checked against `definition.rs`. Upload receipts are
[artifact-upload-receipts.md](../interfaces/artifact-upload-receipts.md); metadata compatibility is
[metadata-schema.md](../interfaces/metadata-schema.md).

## Entities and state

A cache decision uses a complete computation identity and conservative dependency provenance.
Validated proofs are rebuildable evidence scoped to a logical node, layout and metadata
generation. A run has completed artifacts, queued recording, committed progress and a
terminal outcome; process ownership can be proven alive, proven gone or unknown.

A node's **cache state** is the outcome of the decision `barca status` and `get --dry-run` make
from the same function. It is one enum, `CacheState` in
[cache_state.rs](../../crates/barca-model/src/cache_state.rs) (0.39.5, #802), used by status
JSON (`cache.state`), the `--state` filter, the server's node state and, through generated
TypeScript types, the UI; nothing else spells a state. Its eight values, in the order the
manual, the OpenAPI `Cache.state` enum and `summary` list them:

| Variant | JSON spelling | Meaning |
| --- | --- | --- |
| `Cached` | `cached` | a successful result matches this code and these inputs; `get` serves it |
| `Unsynced` | `unsynced` | cached, but not where it should be: local copy, store object, digest or server ledger disagree |
| `Stale` | `stale` | it ran before, but `get` would run it again |
| `NeverRun` | `never_run` | no successful materialization is recorded |
| `Partial` | `partial` | a partitioned asset with some keys cached |
| `Blocked` | `blocked` | a partitioned asset whose missing keys wait on failed keys of an input |
| `Unknown` | `unknown` | not decidable before something upstream runs (dynamic partitions, sensor output, cutoff, not planned) |
| `AlwaysRuns` | `always_runs` | tasks and sensors are never cached |

Parsing is strict (`never-run` is not a state; the table view prints that spelling itself);
decoding JSON from a newer producer is lenient, an unknown string becomes `unknown`, so a
client of a newer server still has a state and its `reason` and `detail` still say why. The
reasons that accompany each state are the manual's (`barca docs status`, "Cache state") and the
OpenAPI `Cache.reason` enum; a test keeps the code's states and reasons equal to those enums.

### The rules the prediction follows (#985)

A run (`get`, `run`, a scheduled tick) reads a held node as it is and never computes it: a
`Manual` asset or task that is not named in `--refresh`, and an asset pinned with `barca pin`
([freshness](freshness.md), "Freshness policy"). The status prediction (`barca status`,
`GET /state`, `GET /nodes`, `barca.nodes`, a node's partition grid;
`RunRules::status` in `crates/barca-decide/src/freshness.rs`) decides by the same rules:

- **Every node in scope is decided**, the held ones and what is above them included. A run
  plans nothing above a held node; status still reports those nodes.
- **A held node is decided on its own**: against its current code and what its inputs would
  give. It reads `stale` when they moved on, though no run will recompute it: only a person
  does (`--refresh`, `barca run` for a task, `barca unpin`).
- **What reads a held node is decided against the result a run reads**: the `Manual` node's
  latest successful result, or the pinned materialization (its run hash, and its output hash
  where the node folds its output). So every node below it has the state, and the run hash,
  that `barca get --dry-run`, a `get` and a tick find. A `cached` node below a held node that
  is stale keeps `reason: "materialized"` and names that node in `detail` (`up to date against
  'baseline' as it is: ...`, with the command that recomputes it); the nearest stale held node
  above is named, through any number of nodes in between. A held input is never the cause of
  `upstream_stale`.
- Nothing the held node's own prediction cannot tell passes to its readers: a held node that
  is `unknown` (a sensor above it has no recorded output, a cutoff asset above it would run)
  or a held cutoff asset that would run leaves what reads it decided.
- **A held node with nothing to read** (a `Manual` node never computed, a pin whose
  materialization is gone) holds nothing: a request stops there with exit 2 and a tick blocks
  the targets that read it, and status decides what is below as if the node were computed
  (`never_run`, `stale`).

What still differs between the prediction and a run, by design:

| Case | A request (`get`, `run`, `--dry-run`) | A tick | Status |
| --- | --- | --- | --- |
| The held node itself | `cached`, detail `Manual and stale: ...` or reason `pinned` | the same; it is never a target | its own state (`stale` when it moved on) |
| Nodes above a held node | not planned | not planned | decided |
| A node paused with `barca pause` | recomputed when it is read; refused as a target without `--allow-paused` | read as it is; skipped as a target | as a request: what is below a stale paused node is `stale`, and the tick's outcome is `last_check` ([health](health.md), "Last check") |
| A task other than the scheduled one | runs | fires only when it is out of date | `always_runs` |
| A sensor | polled | polled | `always_runs`; what reads it is predicted from its last recorded output |
| A partitioned `Manual` node | computed | computed | computed (it is not held) |
| A pin on one complete partition key | read as pinned | read as pinned | read as pinned |

Conformance: `python/tests/test_status_run_rules.py` (a stale `Manual` node above `Always`
assets with and without a cutoff asset in between, a pinned asset, a paused node, against
`--dry-run`, a `get` and ticks of a live scheduler), `status.rs` (`states_and_reasons`).

## Rules and transitions

Product observation/membership rules are owned by [freshness](freshness.md), run/batch/process
semantics by [execution](execution.md), and status meaning by [inspection](inspection.md).
[The approved replacement](current-state-and-execution.md) is not implemented behavior.

### Validated freshness before expansion

Default cache-aware execution may omit proven-fresh work before per-key expansion. Proofs
are private, rebuildable metadata, scoped to each requested logical node and artifact layout.
They contain the node's structural signature (definition, bindings, freshness, sinks,
partition specs, declared environment and upstream signatures), its validated key count,
and compact source observations or an unpartitioned output reference. Cutoff and Manual upstreams
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

Workers compare source snapshots against digest-verified previous artifacts and report
only affected keys and membership changes. Rust retains counts and digests, projects
changed keys into expansion, and preserves canonical per-key fingerprints. Uncertain
previous bytes fall back to normal validation. Full snapshots are not persisted inside
freshness proofs. Metadata generation uses an indexed last-row read. Routine cache-size
estimates inspect appended rows; a full survey corrects reused paths before eviction.
Scheduling loads logical priors first and exact key estimates only for affected work.

Source discovery/comparison can be linear in source keys; changed fan-ins necessarily load
all collected inputs. No constant-time claim applies to those operations. The normal complete
hash requirement below applies to all affected work, including lazily loaded fresh inputs.



### Complete upstream hashes

Before deciding a consumer, the coordinator checks that every upstream step in the expanded
plan has a run hash, including every upstream partition key across worker chunks. An
incomplete hash map fails as an infrastructure error (exit 3) before any consumer hash is
computed or any cache row is looked up: a result is never reused on a hash computed from a
partial set of upstream keys. The expected keys come from the selected expanded plan. Worker
pool size does not change valid hashes or cache decisions.

### Conservative provenance

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

### Early cutoff

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

### Keyed refresh scope

A refresh selector `asset[dimension=value]` forces matching keys while preserving the planned
target domain. A comma-separated CLI list (or request array) unions selectors into a set.
Each selector contains one coordinate. Bare names select all keys. Invalid syntax and
selectors matching no planned key fail with a usage error. Per-key cascade follows the same
aligned-or-fan-in upstream lookup as run hashing and stops at cutoff outputs. Unchanged
cutoff bytes preserve downstream cache hits. Complete fan-ins read every domain key; normal
validation/recovery computes missing or stale siblings or blocks publication on failure.
Batching operates on the due keys. `--partition` retains its existing narrowing semantics.
Conformance: `python/tests/test_keyed_refresh.py` (CLI and server, warm sets, cutoffs, batching,
complete publication, invalid keys and missing siblings).

### Recording progress

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

### Explaining a run's executions

- When a run decides a phase, every step that is not a cache hit gets the reason the decision
  has for it, and the run keeps one bounded aggregate of them (`runs.plan_reasons`): by
  category and subject, how many nodes and steps. Categories name only what the engine
  decides: a task or sensor, a refresh (named, cascaded, all), a missing or evicted artifact
  read by the run, and, for a step with no cache hit, its history: no success ever (never run,
  or new keys of a partitioned node), the last attempt at this run hash failed, or the
  components that differ from the last success (the `barca status` diff: code, decorator,
  helper, kind, input, sensor, value, env). A last success without recorded components is a
  plain `changed`; a step not looked up (a server's ledger answered, or past 10,000 keys of one
  node in a phase) is `not_examined`.
- Only steps that run are looked up, a few thousand per query, so a run served from cache pays
  nothing; the aggregate is written by the progress recorder with its next batch and again with
  the terminal write. The reasons explain a decision; they never make one.
- A run in progress has the reasons of the phases decided so far. A phase never reached (the run
  failed or was cancelled first) adds nothing.

### Interrupted runs

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

### Operator-held partition data

A pin can name one complete partition key. The key's materialization identity is its pinned
run hash, even under refresh, and aligned consumers fold in that hash. Other keys retain
their normal decisions. Static and expanded runtime keys used only through a pinned key do
not need computation. Runtime membership sources remain authoritative: a pin never adds a
key to their output. Uploaded replacements validate the selected key before recording data.
The grid distinguishes a held key (`p`) from a fresh computed key (`c`), and key inspection
reads the held version even when a newer materialization exists.

## Invariants

- **INV-CACHE-HASH-01:** A consumer cannot use a cache decision based on an incomplete set
  of required upstream hashes.
- **INV-CACHE-DURABLE-01:** Replay does not duplicate a run/step result or progress count;
  required terminal result rows and status commit together or leave prior committed state.
- **INV-CACHE-OWNER-01:** Uncertain process ownership alone cannot relabel a run interrupted;
  explicit operator reconciliation refuses a proven live owner.
- **INV-CACHE-EXPLAIN-01:** A recorded plan reason never changes a cache decision, and every
  step a run decided to execute is counted under at least one reason of its aggregate.

## Failures and recovery

An incomplete upstream identity fails as infrastructure failure before cache lookup.
Malformed or missing rebuildable proofs fall back to validation; database errors remain
infrastructure errors. Failed progress batches retry without claiming uncommitted progress.
A required terminal-write failure preserves earlier committed state and reports failure.
Owner uncertainty remains running unless explicitly reconciled by an operator.

## Acceptance and evidence

Exercise incomplete hashes across worker chunks, conservative bindings and imports, cutoff
propagation, invalid/missing proofs, repeated progress batches, terminal rollback and unknown
versus proven-live/gone owners. The existing conformance sources below cover their stated
implemented boundaries; ordered publication and shared ownership have separate obligations.

Conformance: cache decision tests in [cache.rs](../../crates/barca-decide/src/cache.rs)
(`partial_upstream_cannot_hit_a_persisted_partial_cache_row`,
`complete_decisions_retain_hashes_across_worker_chunks`,
`aligned_key_present_does_not_allow_another_expanded_chunk_to_be_missing`),
[conditional bindings](../../python/tests/test_conditional_helper_hashing.py),
[helper star imports](../../python/tests/test_helper_star_hashing.py),
[pipeline star imports and binding proof](../../python/tests/test_static_barca_bindings.py),
[unproven decorators](../../python/tests/test_decorator_names_not_barcas.py),
[progress recording](../../python/tests/test_incremental_persist.py),
[progress counts](../../python/tests/test_progress_counts.py),
[terminal outcomes](../../python/tests/test_terminal_persist.py),
early cutoff in [cache.rs](../../crates/barca-decide/src/cache.rs) (`cutoff_tests`),
[cone.rs](../../crates/barca-python/src/cone.rs)
(`a_cutoff_asset_reached_by_name_adds_a_marker_not_its_code`) and
[test_asset_cutoff.py](../../python/tests/test_asset_cutoff.py),
persistence tests in [persist.rs](../../crates/barca-run/src/persist.rs),
run owners in [run_owner.rs](../../crates/barca-db/src/run_owner.rs) and
[reconciliation](../../python/tests/test_history_reconcile.py).
Plan reasons: the tally in [plan_reasons.rs](../../crates/barca-decide/src/plan_reasons.rs) (bounded
entries, nodes counted once) and [real runs](../../python/tests/test_plan_reasons.py) (never run,
a changed helper and input, new keys, a failed retry, `--refresh-all`, a cached run recording
none).

Conformance: [incremental DAG regressions](../../python/tests/test_incremental_dag.py),
[missing artifact recovery](../../python/tests/test_missing_artifact.py), dataset sink,
cutoff, telemetry and startup suites; [benchmark](../../benchmarks/incremental_dag/bench.py).

Conformance: [partition pin recovery](../../python/tests/test_partition_pins.py) exercises static and runtime-derived keys, downstream repair, refresh, unpinning, unknown-key rejection, and HTTP rollback inspection.

## Open questions and gaps

The concurrent-version ordering gap remains documented in [freshness](freshness.md).
Ordered current selection, shared computation ownership and bounded full-domain retained
state are approved requirements in [ownership](current-state-and-execution.md), not proved
by the current cache engine or its warm incremental benchmarks.
