# Static Barca bindings (#316, P07)

## Scope and contract

Reuse `BarcaNames`, the existing conservative module binding collector, as the
single resolver for node/decorator/helper extraction, argument checks and hash
rules. Resolve top-level `import barca [as b]` attributes and
`from barca import name [as alias]` to canonical exported names. Existing direct
imports and star imports remain supported. No module import or execution occurs
while planning; no flags, Python APIs or parser replacement are added.

Explicit foreign or rebound bindings must not define Barca nodes or receive
Barca helper semantics. Retain legacy bare spellings in source snippets without
imports only when no competing binding is present. Provenance-dependent
argument validation remains limited to proven Barca bindings. Namespace
reflection and uncertain writes invalidate positive provenance conservatively;
hashing retains the existing conservative module fallback where applicable.
Module export writes invalidate the affected canonical export across every module alias
and direct import, including imports preceding the write. This deliberately conservative
rule avoids a new statement-order model; unaffected exports through other module aliases
remain recognized.

All existing helpers read from decorators use the same resolver: sink, unsafe,
freshness markers/Schedule, partitions/partitions_from, collect and asset_ref.
Task-body parallel/parallel_map recognition uses module provenance restricted by
function-local bindings so parameters and local assignments cannot masquerade
as imported helpers. Arbitrary dynamically installed exports and runtime alias
assignments are outside static recognition. The local binding collector currently also
treats comprehension targets and lambda parameters as shadows of the enclosing function;
this can omit task helper metadata but does not alter task execution. Precise nested-scope
resolution remains outside this bounded change.

## Implementation sequence

1. Extend the existing binding table with local-name to canonical-name mappings
   and module aliases; preserve its rebound/reflection safeguards. Test aliases,
   foreign imports, assignments, module attribute writes and unknown attributes.
2. Thread the resolver through extraction and validation. Share decorator
   classification with definition hashing, canonicalizing proven Barca names
   while preserving source/body hashing and conservative foreign wrappers.
3. Cover helper extraction and partition hash rules with the same provenance.
   Add node/metadata/hash and task local-shadowing regressions.
4. Exercise real CLI list/plan/get and unknown-argument failures using qualified
   and aliased imports; prove planning never imports a module with side effects.
   Update manual/site documentation and relevant help examples without changing
   command/output schemas. Run focused suites, then core/CLI/lint validation.

One bounded PR owns this resolver/extraction change. P06's lexical unused-input
collector remains independent; rebase after #349 if necessary. Runtime monkey
patching, discovery and a full Python symbol-table engine remain separate work.

## Release compatibility

Breaking: foreign or explicitly rebound decorators merely named asset/sensor/task
no longer define Barca nodes. Import the real Barca decorator (qualified and
aliased forms work), or stack a foreign wrapper on a genuine Barca decorator.
Previously ignored imported aliases now receive the normal argument checks.
Record this in the next 0.21 minor release notes. Reflection-only uncertainty
retains node discovery while validation/hashing keep their conservative fallback.

## Verification

- Workspace Rust tests: 779 passed, including the grammar and repository example sweeps.
- Workspace Clippy with warnings denied, Rust formatting and pinned Ruff passed.
- Actual CLI/decorator/cache/manual/contract suites: 353 passed; the two examples
  initially skipped for optional Polars then passed after installing it (355 total).
- Qualified/module/imported aliases plan without importing side-effectful user code,
  execute partitioned results, and reuse cache after metadata-only edits.
- Explicit foreign/rebound fixtures now assert non-discovery; cache-safety fixtures
  stack their custom wrapper on a genuine imported Barca alias and retain their
  implementation/order/argument invalidation checks.

Review follow-up: explicit export-mutation regressions cover two module aliases,
imports after a write, and reuse of the original module alias for a competing import.
Partition helpers lose Barca metadata/hash treatment across all aliases; a mutated
foreign sink wrapper executes without creating a sink and invalidates cache when its
implementation changes. The focused actual CLI resolver/foreign-decorator/cache suites
passed 264 tests, and the final static-binding suite passed 11 tests after the final build.
