# Crate split (#905)

Status: done, 2026-11-13. Non-normative record of how `barca-core` (94k lines, about 70
top-level modules) became five crates, with no change to commands, flags, JSON keys, routes,
decorator arguments, output or UI. The plan is
[#905](https://github.com/barca-orc/barca/issues/905); the crate map and dependency direction
live in [CLAUDE.md](../../CLAUDE.md).

| Crate | Holds |
|---|---|
| `barca-model` | shared values, ids, hashes, time, `term` (`outln!`/`errln!`), the `barca.toml` config |
| `barca-compile` | source in, `Dag` and `ExecutionPlan` out: parser, decorator arguments, DAG, planner |
| `barca-db` | the metadata database: schema, every write's owner, run recording, run ownership |
| `barca-engine` | the per-step scheduler and worker loop: coordinator, dispatch, I/O loop, protocol, wire, helper processes, batches |
| `barca-core` | wires them: `execution` (the run orchestrator), cache policy, freshness, stores, ledger, status, reports, commands |

`barca-compile` and `barca-db` use `barca-model` and not each other; `barca-engine` uses
`barca-model` and `barca-compile`, never `barca-db`; `barca-cli` and `barca-server` use only
`barca-core`, which re-exports the lower crates' modules under their old paths.

## Steps

- Step 1, layering guard: [#906](https://github.com/barca-orc/barca/pull/906).
- Step 2, upward edges cut inside one crate: [#907](https://github.com/barca-orc/barca/pull/907)
  (execution placed in core), [#908](https://github.com/barca-orc/barca/pull/908) (dispatch,
  results and root error types down), [#909](https://github.com/barca-orc/barca/pull/909)
  (`pid_alive`, host identity), [#910](https://github.com/barca-orc/barca/pull/910) (db value
  types and DDL), [#911](https://github.com/barca-orc/barca/pull/911) (compile-side helpers),
  [#912](https://github.com/barca-orc/barca/pull/912) (config).
- Step 4, physical split: [#913](https://github.com/barca-orc/barca/pull/913) (`barca-model`),
  [#914](https://github.com/barca-orc/barca/pull/914) (`barca-compile`),
  [#915](https://github.com/barca-orc/barca/pull/915) (`barca-db`),
  [#916](https://github.com/barca-orc/barca/pull/916) (`barca-engine`).
- Step 5, `barca-cli` and `barca-server` through `barca-core` only, and step 6, this close-out,
  followed on the same integration branch.

## Decisions that differ from the plan

- **`execution` stays in `barca-core`.** It is the run orchestrator: cache decisions,
  freshness, publication, backup. `barca-engine` is the per-step scheduler and worker loop.
  The engine reaches no metadata at all, so step 3 (traits between execution and the database)
  was unnecessary and was not done.
- **tokio below core.** `barca-model` depends on tokio (`rt`, `time`, `sync`, `macros`) for
  `process::Until`, `CancellationToken` and the task-local output streams; `barca-compile` uses
  tokio `rt` for `spawn_blocking` in `load`. The plan's "no tokio in barca-model/compile" does
  not hold. The rule that matters does: only `barca-db` and `barca-core` link libsql.
- **The attached ledger is opaque below core.** `ResolvedConfig.ledger` is a
  `config::AttachedLedger` holding a `dyn Any`, downcast once per run in core, so the config
  (in `barca-model`) does not name the ledger client (in `barca-core`).
- **The layering guard became a manifest check.** Once each layer was its own crate, Cargo
  enforced every module edge the guard checked. `crates/barca-core/src/layering_tests.rs` now
  only checks the `Cargo.toml` dependency lists against the allowed crate graph and that libsql
  stays in `barca-db` and `barca-core`: Cargo enforces whatever the manifests say, and nothing
  else stops a manifest edit from adding an edge.

## Measured

Release builds (fat LTO), `hyperfine -N`, interleaved runs; numbers from each PR body.

- Release binary: 17,449,840 bytes before #913, 17,504,816 after #916 (+0.3%).
- `benchmarks/trivial` (barca rows): within noise at every step, e.g. #913 53.3/55.1 ms →
  53.6/54.8 ms, #914 44.9/45.4 → 44.8/44.7 ms, #915 49.7/50.9 → 49.8/50.2 ms, #916
  43.0/42.4/42.1/43.4 → 42.4/42.3/41.8/41.9 ms. Absolute values differ between PRs because
  machine load and pinning differed; each pair was measured together.
- `benchmarks/chain_100` (#916): 167.2/165.0/166.4/166.9 ms → 163.4/163.7/164.1/165.9 ms.
- `scripts/perf-gate.py` (#916, `TRIVIAL_RUNS=40`): trivial wall median 39.7/39.5 →
  39.5/40.3 ms, coordinator CPU 7.1/7.8 → 7.2/7.6 ms; all budgets met.

## Follow-ups

- **Pass the ledger through `ExecuteRequest`** instead of the `dyn Any` in `ResolvedConfig`.
- **Broken intra-doc links.** Modules that were crate-private became `#[doc(hidden)] pub` in
  their new crates, and some rustdoc links now point across a crate boundary. No CI job runs
  `cargo doc`, so these are not caught.
- **Always-compiled test hooks.** `cfg(test)` items do not reach a dependent's tests, so the
  hooks core's tests use (three items of barca-model's test-hooks module, `dispatch::expand_pending_partitions`,
  `StepTrace::context`) are compiled into release builds, marked `#[doc(hidden)]`.
- **`scripts/check-fast.sh` diffs against `origin/main` by default**, not the PR's base, so on
  an integration branch it picks Python tests from the whole branch unless given
  `--base origin/<branch>`; it could read the base from the open PR.
- **Executing an externally built plan.** The engine API is sufficient on its own, but the
  phase loop, step recording and run-hash decisions live in core's `execution`, so a plan built
  elsewhere still needs core to run it. Whether barca should do this is a product decision,
  parked per [DIRECTION.md](../../DIRECTION.md).
