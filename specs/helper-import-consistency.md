# Helper hashing and worker import consistency (#295, P08)

## Boundary and reproduced failures

Core owns static dependency cones; Python owns ordinary source imports. A run hash
must describe the implementation executed regardless of worker history, upstream
cache hits or pool size. Static planning must not import Python. Preserve the
accepted shared DuckDB connection and import-time setup per worker process, source
hash-validated bytecode, qualified package module identity and artifact passing.
No new flags, configuration, setup hooks or parser replacement are proposed.

At main 7868138 the real CLI reproduces three failures:

1. `a/p.py` and `b/p.py` import different sibling `helpers.py` modules under the
   same short name. With one worker and `b` depending on `a`, cold execution returns
   `[11, 11]`; refreshing only `b` returns `[11, 22]` under the identical run hash.
   The cached result therefore depends on worker history, despite each static cone
   hashing its own sibling helper.
2. A top-level `try: from helpers import value` works, but editing the helper from
   eleven to twenty-two leaves the asset cached at eleven. Refresh-all returns
   twenty-two under the unchanged hash.
3. `a/p.py` importing `b/shared.py` by the stem `shared` works after an upstream
   asset in that file executes. If that upstream is cached, refreshing the consumer
   fails with `ModuleNotFoundError`. Static fallback resolution depends on the
   discovered pipeline set rather than a supported Python import path.

Evidence: `/tmp/barca-p08-reproduce.py` and its `.log`, using the unchanged helper
resolver from main with this branch's Python sources, the real coordinator and
workers. Promote these reproductions into permanent regressions with each fix.

## Sequential slices

1. **Conservative module-level binding cones.** Reuse the existing scope Binder,
   import collector and Code/Uses representation. For module-level control-flow
   `if`/`try` bindings that cannot be selected statically, hash the module source
   and follow its possible project imports and references conservatively. Do not
   evaluate conditions or guess which branch executes. Preserve selective cones
   for ordinary unconditional imports and definitions. Test conditional imports,
   fallback definitions, qualified conditional module aliases and helper edits,
   plus unrelated unconditional helpers remaining cached. This bounded slice
   needs no worker lifetime or public API change. Whole-module fallback preserves
   earlier bindings when a branch is inactive and fallback definitions when an
   import fails, without adding a new public binding variant. Preserve that
   provenance through every final binding kind: later assignments can consume
   the prior value (`value = wrap(value)`), while earlier aliases can capture it
   before a subsequent function, class or import replaces the original name.
   Retain the fallback even after an unconditional replacement rather than add
   order analysis. Preserve Function/Class kinds and include the module source
   when the conditional name becomes an entry function. Coordinate the existing Barca
   recognition rules with P07 rather than duplicating them.
2. **Deterministic import policy, decision before implementation.** A universal
   `sys.modules` reset/reload is ruled out: it loses supported import-time setup,
   changes custom type identities and may break pickle artifacts. Swapping caches
   by import context introduces a new lifecycle and does not alone solve lazy
   imports or pickle identities. The simplest safe candidate is to reject
   detectable conflicting project-module short names and off-path pipeline-stem
   imports during DAG loading, before user code or run metadata, and require
   ordinary qualified package imports for those layouts. That changes the manual's
   advertised stem-import workflow, so obtain explicit policy approval before
   implementing it. Keep supported unambiguous sibling/root imports unchanged.
   If a compatible deterministic runtime design is selected instead, specify its
   identity, lazy-import, pickle and DuckDB setup guarantees before coding.
3. **Detectable static-cone limits.** Audit each original issue case against the
   current AST visitor; do not treat old descriptions as reproductions. Fix small
   gaps using the existing visitor and conservative fallback, or give existing
   usage diagnostics for clearly unsupported project constructs. Keep dynamic
   imports, arbitrary external import paths and external packages explicit
   documented limits. Do not close #295 until all remaining requirements have
   evidence or a named residual owner.

## Compatibility, errors and concurrency

Slice one changes affected code hashes, causing affected assets to recompute once;
it preserves history and existing artifacts. Unaffected unconditional cones should
retain their hashes. No result fields, error kinds, public hash signatures or
worker-budget behavior change. Additional hash invalidation is conservative:
inactive branch helpers can invalidate because static analysis cannot execute the
condition. Cancellation remains coordinator-owned and no extra processes start.

Slice two requires an explicit compatibility decision and a minor-release breaking
note if advertised layouts are refused. New diagnostics use the existing usage
error envelope and exit two. Never silently choose an implementation based on task
order or accept a wrong cached result.

## Acceptance evidence

Run focused Rust cone/import/layout tests, actual CLI cold/warm/helper-edit
regressions with pool sizes one and two, and worker source-import/pickle tests.
Prove plan/list/dry-run execute no user code for ordinary static cases. Preserve
P02's DuckDB connection and import-time setup tests. For the selected deterministic
policy, vary which upstreams are cached and the order modules reach a worker; the
result and run hash must agree, or the layout must fail before any execution.
Run workspace tests and Clippy, Ruff and relevant CLI/manual contracts. Update the
manual and site together for implemented behavior; do not document pending policy
as shipped. Imports are read once per command using existing project-module caches;
no project-wide helper crawl or additional Python process is introduced.

## First-slice evidence

The conditional-binding slice passes 782 Rust workspace tests and workspace Clippy
with warnings denied; 157 Python CLI/helper/import/DuckDB/manual/contract tests;
Ruff and cargo fmt; and the 51-page site build. Twenty-eight new CLI cases run at pool
sizes one and two, including a missing primary helper appearing after a cached
fallback. Conditional helper edits change actual run hashes, inactive branches
invalidate conservatively, unrelated assets remain cached and planning produces
no module-import marker or metadata directory. Existing historical hash tests and
accepted shared DuckDB setup/input-cleanup tests pass. The former limitation test
for a function defined inside a module-level `if` is now a positive tracking test.
Plain and annotated later rebindings return twelve cold and twenty-three after
editing the helper, with distinct run hashes and warm cache reuse. Imports inside
an unrelated conditional nested function retain their own scope and do not
invalidate an asset using the different module-level binding. Both checks prove
ordinary static planning leaves the user import marker and metadata absent.
Capture-before-function/class/from-import/module-import replacement tests retain
the helper's provenance after its original name is rebound. A conditional name
later replaced by an asset retains its entry-function cone. Unrelated assets
remain cached after these helper edits, with no user imports during planning.

Build/install uses this worktree's target and editable wheel with the test extra in
`/tmp/barca-p08-venv`. Conditional fallback source/reference collection is initialized
once per module, and existing project file caches still read imports once.
Runtime history/stem-import policy is held for review; #295 remains open.
