# Explicit project imports and stable module identity (#295)

Status: approved mechanics before implementation. Rebased to current main after PR355/362. The user
accepted ordinary qualified/explicit imports for ambiguous and history-dependent
layouts: "yeah I think so - we can also require explicit imports". This is a
breaking workflow clarification requiring a minor release; no flags, setup API,
custom namespace loader, module eviction, or parser rewrite.

## Evidence and boundaries

Actual real CLI evidence is retained outside this worktree:
`/tmp/barca-p08-reproduce.py` and `/tmp/barca-p08-pipeline-identity-reproduce.py`.
Two directories' same-named helpers produce different values with the same hash
in different worker histories. An off-path stem import works only after another
pipeline adds its directory. Root `p.py` loaded as `_barca_p` and imported as `p`
runs setup twice and creates different dataclass types; consumer-only refresh
reading a cached producer fails to import `_barca_p` in a fresh worker.

Conditional hashing is separately fixed by PR358; do not duplicate that slice.
Ordinary valid Python imports, including aliases, relative imports and namespace
packages, remain supported. A pipeline filename that cannot be named in Python
import syntax remains executable by its existing path mechanism.

## Existing provenance (inspected, not presumed)

The executing worker request has `source_file`. Daemon upstream inputs are
artifact paths, or collected `{path, format}` entries; `OutputRef` has no producer
source. Python `api._read_output` likewise reads only artifact path and format.
The static `ProjectCones::SourceFiles` cache does retain canonical reachable
source paths while planning, but no expected-source path/hash registry is sent
to these readers on current main. PR355's current source-import/worker code also
has no such registry. Reuse this cache for validation rather than walking the
project. Do not infer a producer from an artifact basename.

## Proposed sequential implementation

1. Add private import validation using existing AST import bindings and
   `ProjectCones` resolution/cache. Compare imports that would bind one ordinary
   module name to different actual project files across supported entry contexts.
   Record the import site and both source paths; return existing Usage/exit 2
   before expression-partition Python, workers, and run metadata. Reuse normal
   package-before-module and sibling-before-root rules. Give a concrete qualified
   replacement such as `from east.helpers import value`.
2. Remove `ModuleSource`'s implicit cross-directory pipeline-stem fallback and
   extra alternative cone. Activate the executing source's existing import path
   before every task, including tasks whose source module is already cached;
   discard only Barca-added path entries belonging to previous tasks, retaining
   normal external paths. Preserve process-wide imported modules and DuckDB
   connection lifetime. Validate that same-name project bindings cannot depend
   on which worker was assigned a prior task.
3. Canonicalize ordinary importable pipeline identities using normal Python
   module names and the existing SourceHashLoader. Reuse a matching already
   loaded module by source path; refuse a conflicting occupied name rather than
   evicting it. Root `p.py` is `p`; qualified `pkg/p.py` is `pkg.p`, including
   namespace packages and top-level package `__init__.py`. Register legacy
   aliases to the same object when applicable; never execute setup again merely
   to install an alias. Keep path loading for non-importable filenames.
4. Add a narrow private pickle compatibility reader for cold `_barca_*` artifacts
   using `pickle.Unpickler.find_class`, reusing the same canonical loading path.
   Existing normal module references use ordinary pickle behavior. Never rewrite
   artifacts or ledger rows. A loaded legacy alias is reused only after checking
   source identity. Missing/ambiguous legacy identity retains data and reports
   the existing explicit refresh remedy.

## Selected mechanics (approved by root before code)

Reject demonstrated conflicting project resolutions and explicit node-input
import references that rely on off-path project resolution. An unrelated
configured `pipelines/json.py` must not outlaw root `import json`. Remove all
implicit directory retention and cone fallback. Otherwise-unavailable helper-only
imports fail like ordinary Python; there is no installed-package discovery
subsystem or arbitrary restriction on Python import syntax.

Historical names are `_barca_` plus every source path component (without the
`.py` suffix) joined with `__`; outside-root paths historically used just the
stem. A legacy name cannot prove one component boundary by splitting at `__`:
`a__b__p` could refer to `a/b/p.py`, `a__b/p.py`, `a/b__p.py`, or `a__b__p.py`.

The compatibility reader enumerates every delimiter grouping using path-pruned
traversal under the already known project root. At each existing directory,
probe a remaining literal filename and every possible next-directory prefix
ending at a delimiter. Never enumerate directory contents or walk the project.
Canonicalize/deduplicate candidate files and recompute their historical names.
Use a fixed private work budget of 256 filesystem candidate probes per lookup;
if the budget is exhausted before enumeration finishes, refuse even if one
candidate was already found. Uniqueness requires exhaustive proof. Any missing,
ambiguous, outside-root or unprovable legacy identity preserves artifacts and
history and gives existing explicit refresh guidance. Normal Python module
references use ordinary pickle behavior. Reuse planner source caches in static
validation; no new reader registry plumbing is introduced.

A narrow private `pickle.Unpickler.find_class` compatibility reader invokes the
existing checked source import path for a uniquely proven legacy source. Reuse
successful module identity in the process, preserving one setup and DuckDB
connection. A cached-only producer imports once when its class is first needed;
later execution and qualified imports reuse it. Concurrent collected reads use
ordinary Python import locking, rather than racing manual exec_module calls.
Root and namespace/package imports have ordinary canonical identities. Files
without an ordinary importable identity retain existing path loading; no file
naming restrictions are added. Existing source/path claims capture project root
once, so user `chdir` cannot change legacy interpretation.

## Required evidence before completion

- Actual plan/run cold, warm and refresh under pool 1, 2 and default; helper edits
  change hashes/outputs while unrelated cones remain selective.
- Conflict diagnostics precede a top-level marker, expression evaluation,
  worker startup and run metadata; names/files/replacement are concrete.
- Valid root/sibling imports, package relative imports, namespace-qualified
  imports, aliases and installed/stdlib imports remain supported.
- Different worker histories cannot change source selection or class identity.
- Import-time setup runs once per process across pipeline execution and normal
  qualified imports; retain one DuckDB connection and real relation inputs.
- New normal pickles load in fresh workers and Python API readers. Real artifacts
  created by the old binary retain `_barca_p` identity and load through the new
  worker/API without producer execution or data/history mutation.
- Cached-only producer module loads once when required; subsequent execution
  does not repeat setup. Missing/ambiguous legacy cases preserve files and ledger
  with actionable refresh guidance. Include concurrent collected reads.
- Required workspace checks and meaningful CLI tests; keep #295 open until this
  evidence and any remaining explicitly documented limitations are reviewed.

## Implementation evidence and precise boundaries

Actual-site validation also compares a project binding with another actual site's
external/unavailable binding, because resetting sys.path alone does not remove
warm sys.modules objects. Unrelated off-path stems remain allowed. A selected
pipeline and imports of it must share one ordinary root-relative identity;
importing the same file as both a bare stem and a qualified path requires the
qualified form. Unambiguous non-pipeline sibling helpers remain supported.

Validation follows literal imports transitively through existing source caches,
including inactive branches and unused functions. It can conservatively refuse
those imports; dynamic imports and user mutations of sys.path/sys.modules are
not statically proven. Files already loaded by the existing checked path loader
outside the root retain ordinary registered-module pickle behavior when their
source identity is directly known. Cold outside-root legacy recovery still
refuses, because no root-relative historical identity can prove that source.

The actual old CLI created a `_barca_p` artifact at
`/tmp/barca-explicit-real-old-vq9sashl`; new cached-only producer consumption ran
only the consumer step, matched normal `p.Record`, and the fresh public Python
API reader returned the same value/module. Producer execution stayed absent,
artifact SHA256 stayed b29ef0c1d021becb3102c50bfe6ae5b3026f04b674e69daeb44a0311a67bf8f6,
and setup happened once per PID. Reproduction script/log:
`/tmp/barca-explicit-real-old-pickle.py` and `.log`.

Final lifecycle review added per-source setup locks: no global source lock is held
across arbitrary user setup, so a setup thread can ordinarily import a different
helper while the parent waits. Concurrent compatibility reads wait for the same
source's setup to finish. Worker execution restores the consumer's import path
after cached-input recovery imports a producer. Focused actual worker tests cover
threaded setup, cold legacy input followed by sibling and qualified imports, and
setup-once identity; existing DuckDB connection/relation coverage remains required.

Final verification: 796 workspace Rust tests and strict all-target Clippy passed;
205 combined CLI/API/parallel/artifact tests passed, then all 38 focused import
cases passed after the final external-prefix and missing-dependency cases. Both
threaded-setup and consumer-path regressions fail with the old boundaries restored.
The actual old-CLI proof was repeated at `/tmp/barca-explicit-real-old-pkt45llb`
with identical preserved artifact hash and one consumer execution. Website build
produced 51 pages. Pinned Ruff, Rust formatting, version sync and lock checks pass;
`ty` reports no errors and six existing dependency/environment warnings.

Compatibility note: this requires a minor release because layouts that depended
on ambiguous bare stems or prior-worker-directory fallbacks must use explicit
qualified imports. Ordinary installed/stdlib imports and unambiguous sibling
helpers remain supported. An external module with an `_barca_` name retains normal
pickle importing only after exhaustive proof finds no matching legacy project
source; missing dependencies in a proven producer retain their original error.

Independent review found that protocol 4/5 encode nested class names such as
`Outer.Record`; a plain `getattr(module, name)` is not ordinary pickle lookup.
The compatibility reader must recover the proven module, then delegate to the
existing superclass `find_class` with that module's actual identity. This keeps
pickle's protocol-specific qualified-name resolution, including protocols 2/3,
without a custom attribute parser or changes to source proof/permissions. New
actual legacy nested-class artifacts cover protocols 2 through 5, class identity,
setup once and byte preservation; protocol 4/5 fail before this correction.

A second independent setup-thread regression reads an unrelated cached producer
through the existing API while the parent waits for that thread. The global path
bookkeeping lock must cover only path/name bookkeeping, never user imports or
waiting for setup. Ordinary import locks and per-source setup ownership retain
setup-once behavior; explicit path-loader registration also holds only that
source's ownership to avoid exposing another partial alias object. This case fails
against the prior global pipeline lock, then must pass with identity and artifact
preservation. Concurrent readers, source paths, canonical names and DuckDB setup
remain part of the complete regression suite.

Review correction evidence: all 44 focused cases pass, with 168 combined
artifact/API/stale-bytecode/parallel cases passing. Protocol 4/5 nested legacy
class cases and the setup-thread API reader fail before their corrections;
protocol 2/3 remain supported. Concurrent explicit path loads also fail the prior
registration boundary, then return one fully initialized object/setup after the
fix. Actual old-CLI nested `Outer.Record` artifacts additionally pass new worker
and public API consumption with one consumer execution, normal class identity,
unchanged bytes and setup once per PID. Legacy recovery restores the caller's
prior path after producer setup; the threaded consumer can then import its
unambiguous sibling helper. No global lock spans user imports/setup.

### Current-main integration and earlier helper regressions

Rebased cleanly onto main `e110f80` after the complete-upstream prerequisite
merged. The expanded existing helper suite exposed two assertions explicitly
requiring the removed worker-directory history fallback. Those tests now prove
an off-path bare pipeline import is refused before metadata, its qualified
replacement works and hashes the actual source, and a consumer's root helper
is independent of its upstream's previously loaded directory. Both revised
regressions fail the old installed CLI and pass the corrected CLI. All 33
helper-tracking cases pass; unrelated helper and input cones stay selective.

The current-base full Rust workspace passed 812 tests and strict all-target
Clippy. Both real old-CLI artifact proofs passed again, including nested class
worker/public-API recovery with unchanged artifact SHA and setup once per PID.

The rebuilt current-base package also passed all 337 combined actual
import/artifact/worker/API/bytecode/parallel/input/helper/DuckDB tests, including
all 44 focused import cases and the repaired historical-workflow regressions.

### Partial-loader integration after #361 (main `1642da9`)

Actual rebuilt combined-base server regressions show that the first global
import-validation error aborts server startup (exit 3), hiding unrelated healthy
assets and scheduled work. Both conflicting project sites and project-vs-stdlib
sites reproduce this. Fix this within the shared existing loader/validator:

1. Return private source-scoped import diagnostics from the existing validator,
   retaining ordinary resolver, source cache, candidate priority and diagnostic
   text. Strict loading still returns the first diagnostic as Usage (exit 2)
   before user imports, expression expansion, workers or run metadata.
2. For conflicting project bindings, mark both project importing sites. When
   one site is external/unavailable, mark only the incompatible project site;
   preserve the valid stdlib/installed import site. Canonical identity conflicts
   and explicit off-path references mark their actual importing source. Keep
   literal source-wide conservative validation; do not introduce a second
   parser, loader, graph or public API.
3. Partial loading converts diagnostics to existing LoadError entries, excludes
   affected definitions, and passes their source files through existing failed
   source/candidate-priority isolation so dependents cannot silently redirect.
   Healthy unrelated definitions keep scheduling, inspection and execution.
4. Prove qualified watch repair restores both sites and clears diagnostics,
   real healthy scheduling beside conflicts, project-vs-external preservation,
   failed higher-priority candidates, and all original load-isolation/scheduler
   cases. Rerun strict and legacy/setup/import regressions on the merged base.

Both new actual server regressions failed the uncorrected combined base at
startup. This integration is approved and preserves the accepted import policy.
