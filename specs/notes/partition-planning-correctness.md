# Partition planning correctness (#337, P09)

## Boundary and remaining defects

The existing planner, partition dispatcher and shared cache-decision function stay
in place. Planning and execution must use the same existing worker-pool resolver.
Unsupported dimensions must fail while building the DAG, before user imports,
workers, run records or storage synchronization. Reports describe the same keys
and warnings regardless of how the pool divides work. No new flags, configuration,
public result fields or partition modes are needed.

The current code already rejects mixing dimensions when partitions_from reads a
partitioned source, but not when its source is an unpartitioned list asset.
queries::plan still hardcodes ten workers. Warnings traverse stream order and key
summaries truncate before sorting. compute_run_hash remains a permissive low-level
helper; production cache decisions need a complete-upstream invariant at their
boundary. The 20,000-key performance investigation belongs to #345.

## Sequential merge plan

1. **Planning validation and deterministic reports.** Reuse default_pool_size in
   the read-only plan command. Validate unsupported mixed static/runtime-derived
   dimensions centrally in DAG construction, preserving existing supported
   single-dimension and static Cartesian cases. Sort warnings by node, parameter
   and kind. Keep each key preview bounded to twenty while selecting the smallest
   sorted keys before truncation; merge previews using the same rule, including
   missing-artifact recomputation. Update the manual, help and contract to state
   the pool and ordering rules. Test CLI plan/dry-run/run on cold and warm
   partition chains at pool sizes one, two and the default. Prove unsupported
   dimensions exit two without importing user code or starting a run.
2. **Complete upstream cache boundary.** After the first PR is merged, enforce
   availability of every required upstream run hash before decide_step hashes a
   consumer. Derive expected static, runtime-derived, aligned and collected keys
   from the existing planned/expanded step set, rather than guessing completeness
   from any matching map entry. Preserve empty partition collections and sensor
   unknown predictions. Keep compute_run_hash's public signature unchanged and
   use an internal invariant/error path. Add regressions for entirely absent and
   partially present upstream keys, plus cold/warm runtime-derived fan-in across
   pool sizes. Open a separate bounded PR if this invariant needs wider context
   threading through execution and dry-run prediction.

## Acceptance checks

Run focused DAG/warning/report and CLI contract Rust tests; actual CLI partition,
pool-size, dry-run, missing-artifact and manual examples against this branch's
binary and Python package; workspace tests, Clippy and Ruff. Compare stable
warning arrays, sorted twenty-key previews, results and materialization hashes.
The phase/stream shape may vary with the selected pool. Runtime-derived keys may
remain unknown until their source has run; static planning never imports user code.

## First-slice evidence

The first slice passes 772 Rust workspace tests, workspace Clippy with warnings
as errors, and 172 focused CLI Python tests (two optional Polars examples skipped).
The new CLI cases exercise a reversed thirty-key chain at pool sizes one, two and
default; cold and warm results are sixty and all sixty-two materialization hashes
match across pools. Ordinary, forced and missing-artifact previews select the same
first twenty lexical keys. Static and evaluated mixed dimensions fail with exit
two before module side effects or metadata directories for plan, get and run.
Complete-upstream enforcement is still pending in the second slice.

## CI backend follow-up

The first backend run failed three legacy example/derived-partition tests that
assumed a three-key plan always contains three physical steps. With the planning
pool fix, a two-worker runner correctly emits two chunks while executing every
key. Those tests now exercise explicit pools one, two and three, asserting chunk
counts and retaining full result/materialization checks. All 55 affected manual,
derived-partition and planning-contract CLI cases pass. This changes test
expectations only; complete-upstream enforcement remains the pending second slice.
