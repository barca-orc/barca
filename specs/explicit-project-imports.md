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
