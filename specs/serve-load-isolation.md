# Serve startup/load isolation (#312, P11)

## Scope and reproduction

Current main 31c7937 binds the server even with a syntax-invalid sibling, but
`/assets` returns 400 and the scheduler disables every job. A scheduled task in
`good.py` does not tick while `broken.py` fails parsing. `/health` misleadingly
reports configured scheduler availability. One-shot commands correctly remain
strict. Reproduced with an actual CLI process and HTTP requests, no user imports
for the inspection reproduction.

Keep healthy serve pipelines inspectable and runnable, quarantine invalid source
and dependent work, and show actionable source diagnostics at startup and in UI.
This is not worker/session isolation: preserve the accepted sequential shared
DuckDB connection per worker process, existing cache identity, and execution
cancellation. No flags, setup hooks, imports added to static extraction, new HTTP
route, or metadata migration.

## Ownership and proposed bounded implementation

Core owns source extraction, hashing and DAG dependency validation. Add a partial
loader that returns a validated healthy DAG plus structured diagnostics, using the
same extractor, import/name resolver and DAG rules as strict loading. Factor the
existing query/status/plan operations into reusable from-DAG helpers. Streaming
execution preparation accepts the supplied validated DAG through a small shared
core entry point; retain strict existing command entry points unchanged.

Server owns the source-generation snapshot and publication. Every inspection,
scheduler/admission and run uses its healthy DAG; no command reparses the original
unfiltered source into a different graph. Keep original configured files for watch
recovery and source-change checks. Revalidate queued runs before execution.

Syntax/read failures exclude definitions in the failed file. Graph errors exclude
only the invalid definitions and their dependent closure, preserving unrelated
nodes in the same valid module. Existing resolution must not redirect a missing
upstream from a failed source to an unrelated similarly named definition. Duplicate
IDs exclude all competing definitions; cycles exclude the cycle and dependents,
using existing graph/resolver data rather than guessed lexical references.

Actual reproduction: a valid pipeline.py containing blocked(inputs={value:
asset_ref("broken.py:bad")}) and an unrelated scheduled healthy task imports and
healthy() executes, while strict DAG construction rejects blocked. Therefore
whole-file graph quarantine would incorrectly disable supported unrelated work
and is rejected. Static inspection never imports a module to test its importability;
actual arbitrary runtime import errors retain existing run failure semantics.

No-match/duplicate/cycle errors must fail closed and remain visible, never guessed
away. Empty healthy source selection remains an inspectable server with diagnostics
and no scheduled/accepted executable work. Global configuration/storage failures
are not source errors and retain their existing failure behavior.

## Additive HTTP/UI contract (approved representation)

Preserve `/state` as its existing healthy-node array. Add `load_errors` to existing
`/health`, an array of records `{file, error, affected_nodes}`; `affected_nodes`
contains actual node IDs whose definitions cannot be loaded, empty when syntax
failure prevents identifying them. No fake nodes. Generated UI types consume this
single server diagnostic and show a visible banner/list identifying sources and
named affected definitions. Keep existing health fields/route and response shapes.
No new user controls. Update HTTP documentation, generated type and client, help,
manual/site discovery/serve guidance and relevant contract checks together.

## Failure, lifetime and recovery

Refresh under existing source change/watch ownership, coalesce stable reads and do
not hold synchronous locks across awaits. A generation/source change prevents
publishing an obsolete selection. Revalidate source selection before queued runs
execute; broken or removed definitions cannot execute using a stale snapshot.
Already-running workers keep existing execution semantics. Status and execution
refresh helper cones even when configured-file stamps are unchanged; otherwise an
edited undecorated helper could incorrectly reuse an old result hash. Watch repair re-adds
files and schedules; repeated failures do not multiply log spam. No new worker or DuckDB connection. The watch guard owns one coalesced refresh
task (one pending notification) and aborts it when the watcher is dropped. Every
event advances scheduler generation; editor debounce never drops a repair/removal. Parsing/validation cost is bounded by configured
source size and DAG; never loop without removing a validated offending definition
or returning a global diagnostic.

## Regression evidence

- Old binary: valid scheduled task plus broken sibling => `/assets` 400, no tick.
- New binary: healthy scheduled task ticks, assets/state contain healthy nodes,
  health and UI identify broken file and affected dependents; trigger blocked
  target refused before execution, unrelated runnable target succeeds.
- Direct/transitive dependency isolation, ambiguous/duplicate/cycle fail-closed,
  all-broken source set, missing explicit source, no static resolver imports.
- Watch repair restores inspection/admission/schedules, later break removes them,
  queued-run revalidation excludes stale definitions.
- One-shot get/run/list/plan/status retain strict parse/dependency errors.
- Existing server/CLI contracts and generated type drift, focused browser/UI
  diagnostic tests, Rust checks and actual CLI lifecycle checks.

Implementation follows the node-preserving shared core boundary. The additive
health representation is approved; no new user-facing policy controls are introduced.

## Verification checkpoint

Original 31c7937 reproduction returned `/assets` 400 and disabled healthy schedules.
The implementation passes 790 Rust workspace tests, strict workspace all-target
Clippy, 39 actual CLI/server/client tests (10 new isolation/lifetime/hash checks),
UI build/lint and the actual Playwright diagnostic-plus-healthy-graph test. Generated
TypeScript bindings include the core-owned LoadError. Watch tests poll health-only
for repair/removal, verify schedule registry restoration/removal and show repeated
inspection reads do not generate reload work. Source-order preservation retains the
existing ambiguity diagnostic order. Current-main rebase verification follows before
publishing the PR; no release is claimed by this checkpoint.

After rebasing onto c914711 (P06), all 791 Rust workspace tests and strict
workspace Clippy pass. Actual server/client/CLI-contract integration passes 68
checks. No source or dependency policy changed in the rebase. UI build/lint and
Playwright remain validated for the same UI patch.

### Independent review: preserve candidate priority

An actual serve reproduction with broken `shared.py`, healthy
`sub/shared.py:value`, and `sub/p.py` importing `from shared import value`
incorrectly excluded the consumer. The unloaded-source guard checked every
candidate, although the ordinary resolver selects the healthy sibling first.
Evidence: `/tmp/barca-p11-shadow-review-zzrfuh2o` (inspection only, no imports).

Bounded repair: visit candidate files in the resolver's existing priority order.
An earlier healthy matching definition ends the check; an earlier failed source
refuses the reference before it can redirect to a later matching definition.
Apply the same literal-then-relative order to canonical references, preserving
exact canonical-id priority. Add both-direction regression cases for imported
and canonical references, proving healthy admission and genuine redirection
refusal without starting Python. Run targeted core and serve checks. This does
not change import policy or add configuration.

Repair validation: 792 Rust workspace tests, all 12 actual serve-isolation CLI
cases, strict workspace Clippy, Rust formatting, and Python Ruff pass. The
new four-direction core test checks the selected upstream identity as well as
quarantine; serve cases place a raising top-level statement in the healthy
module to prove inspection does not import it.
