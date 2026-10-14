# P09 second slice: complete upstream cache decisions

Status: implemented second slice, specification version 1. First slice PR #354 is merged.
Plan recorded before implementation on main 748479ff; integrated onto ff911226. Refs #337, specs/partition-planning-correctness.md.
Conformance: cache boundary Rust regressions and existing actual CLI partition suites.

## Concrete current-code counterexample

cache::compute_run_hash iterates upstream IDs. If neither a direct nor aligned
hash exists, it collects every hash whose display ID starts with the upstream
base ID. An empty set contributes nothing; a nonempty partial set contributes
only those keys. cache::decide_step currently calls this directly and records the
consumer hash without checking completeness. Dependency-order traversal fixes the
existing caller, but does not establish the invariant at the decision boundary.

A regression should construct an actual three-key upstream phase and its
collect() consumer, seed only upstream keys a and c in DecideState.run_hashes,
then call decide_step on the consumer. Today it accepts a hash excluding b.
After the change it must return an infra error naming b before adding any consumer
hash or looking up its cache row. Repeat with no upstream hashes and with an
aligned consumer whose own key is present while another upstream chunk is absent.
The complete case must retain exactly today's run hash. Do not change the public
compute_run_hash helper: its intentionally permissive low-level contract stays.

## Private context lifecycle

1. Before deciding each actual ready/expanded phase, register its complete display
   IDs in private DecideState bookkeeping grouped by base node. Derive IDs from
   actual StreamStep.partition_keys; use the existing display ID for unpartitioned
   steps. Include every stream before the first consumer decision, independent
   of pool size. Retain previous phase manifests; repeated registration unions
   IDs rather than replacing a complete manifest with a recovery subset.
2. Registration belongs inside shared cache-decision infrastructure, called by
   both real execution and dry-run prediction after expand_pending_partitions.
   Dry-run partition-unknown steps stay excluded and propagate their existing
   unknown reason before attempting any consumer decision. Do not read runtime
   source artifacts again or infer a manifest from existing hash-map contents.
3. decide_step checks the registered upstream display IDs against run_hashes
   once per upstream input before hashing any consumer key. Missing registration
   is an internal invariant failure too. Use sorted manifests/upstream names to
   make diagnostics stable. Hash presence is distinct from sensor-output presence:
   existing sensor-unknown predictions keep their current meaning.
4. Return the existing BarcaError::Other/infra path, propagated through private
   predict_steps/predict_phase helpers and the real decision loop. Public Python,
   command, result and compute_run_hash signatures stay unchanged. No assertions
   that would panic on a recoverable coordinator invariant and no new public
   error classes, flags or configuration.

## Tests and scope

Use the actual static and runtime-expanded phase builders to test missing-all,
missing-one fan-in, missing-one aligned phase, registration omission, full maps
and pool chunking. Prove refusal leaves consumer hashes absent. Cover previous
phase and current phase dependencies, repeated recovery-subset registration,
sensors whose output is unknown and existing empty-key behavior; do not invent
empty-key semantics to implement this guard. Run existing pool-size/partitions_from
cold/warm/edit fan-in suites at one, two and default pools against the new binary.

The manifest costs one bounded-by-plan set of key identities per run. Run hashes are private and monotonic within DecideState, with an immutable accessor
for other coordinator components. Cache successful completeness checks by upstream;
registration of new identities invalidates that upstream memo. Thus consumers and
chunks share one full scan per unchanged manifest, with no manifest clones per
consumer or per-key scans. Manifest storage is O(actual expanded identities), and
registration visits those identities once per actual phase. Recovery subset union
never removes identities. General hash lookup indexes and 20k-key profiling belong
to #345. This is an invariant guard, not an Engine
refactor or new cache interface.

## Current-code refinements before implementation

The only decide_step callers are real decide_phase and prediction predict_steps.
Register all expanded phase identities inside the existing DecideState before the
first decision in either path. Unexpanded pending-partition placeholders are not
registered; prediction already propagates partitions_unknown before reaching a
consumer decision. No new planner/context owner or additional artifact reads.

Use a private base-ID to sorted display-ID set, unioned across phases/recovery.
Validate each distinct input upstream once per step/chunk, before computing or
inserting any consumer hash. A missing manifest or missing required hash returns
the existing infra error path naming the consumer and first deterministic missing
upstream key; no unbounded diagnostic list. Propagate Result through existing
private prediction helpers. Sensor output uncertainty remains separate from
run-hash completeness and preserves current unknown/forced-run reporting.

Current dispatch keeps an expanded zero-key step with an empty partition_keys
vector, which existing hashing treats as its base display ID. Register that actual
identity, preserving existing behavior rather than introducing a new empty-key
collection contract in this invariant patch. Raw compute_run_hash remains permissive
and unchanged, including its historical hash tests.

First prove an actual false cache decision: build a three-key planned upstream
with collect consumer, seed only a/c hashes, persist a successful consumer artifact
under the incomplete low-level hash, and call the real decide_step/CacheReader.
The pre-change boundary should return Cached while b is absent; the fixed boundary
must reject before recording any consumer hash. This is a decision-boundary fault
injection regression, not a claim that the now-correct dependency-order caller
naturally drops keys. Tests then cover missing-all, aligned partial chunks, absent
registration, complete hash equality across pools, expanded phases, previous-phase
identity retention, recovery subset union and sensor/unknown/empty-key compatibility.

No public compute_run_hash, command, Python or result signature changes. No flags,
new error classes, storage ownership or version bumps. Manual/site may explain the
internal completeness guarantee; output contracts remain unchanged. The 20k-key
profiling/indexing work remains #345.

## Reproduction and implementation evidence

The pre-change real decide_step/CacheReader test returned Cached for a persisted
consumer under partial hash
`10edaca246636b32f1d0198914c528ff9412d97197482de66540f50d72c2731b`
while `t.py:part[k=b]` was absent. The complete low-level hash was
`6e4115c856f3fefd9a13bfce530e1c6e0aa6e6acd5284271c006d4766b14a9f1`.
The regression now requires an infra error naming b, an absent consumer hash,
and independently confirms that the tempting partial cache row still exists.
This remains a private boundary fault injection, not a reproduction of key loss
from the dependency-ordered current CLI.

`cache::upstream_boundary_tests` also proves missing registration versus wholly
absent upstreams, sorted first-missing identity, selected subset completeness,
recovery union and added-key memo invalidation, aligned chunk refusal despite
refresh/no-cache, complete hash equality across pools 1/2/64, actual runtime
expansion with previous-phase keys, and preserved zero-key base-step identity.
The expected manifest is independent of map contents and unions actual expanded
phase identities; it is private, as is the monotonic run-hash map.

Validation on ff911226: all 795 Rust workspace tests pass, including six new
boundary regressions. The actual CLI passes 127 partition/pool/runtime-key/cache/
sensor/missing-artifact tests and 64 manual/CLI-contract tests against this
worktree's rebuilt binary and source. Workspace Clippy with warnings denied,
Rust formatting, lock consistency and whitespace checks pass; the site builds
all 51 pages. Cold/warm/edit and dry-run cases cover pools one, two and default
(existing pool-independence cases also cover 3/5/16). No hash-format migration.

Review correction: StreamStep.inputs already contains canonical base IDs copied
from DagNode.resolved_inputs/resolved_collected. The guard must compare these
exactly, without splitting '[' (valid in source filenames/directories). An actual
pre-fix bracketed filename dry run returned infra exit 3; regression cases cover
bracketed file/directory combinations at pools 1/2/default across cold/warm/get/run
and dry-run execution. A boundary regression additionally retains full bracketed
canonical IDs for partition manifests, diagnoses the missing b key and preserves
the complete historical low-level hash. No public hash or filename contract change.
