# Reliability contract consolidation plan

Status: documentation-only plan, 2026-10-10. Base: main `96022e0`.
Owner: roadmap [#344](https://github.com/barca-orc/barca/issues/344).

The ownership, durability and result-identity rules are distributed across
historical implementation plans. Consolidate the current guarantees and gaps
without changing runtime behavior or presenting proposed APIs as implemented.

1. Inventory shipped resource lifetimes and commit boundaries against existing
   implementation owners and regression tests.
2. Add a single Markdown contract index with implemented/proposed status,
   independent document version, evidence links and explicit limitations.
3. Separate computation keys, artifact-byte checksums, durable run identities
   and current partition membership. Record the accepted asset-purity and
   computation-version model; exact saved-result selectors remain unresolved.
4. Link the index from `specs/README.md`; preserve native HTTP, CLI, metadata and
   Python grammar specifications and historical implementation evidence.
5. Define bounded PR sequencing and issue ownership. Existing correctness fixes
   may proceed independently; saved-results implementation requires its exact
   API, version-selection and retention specification. Do not introduce an
   archive of every execution's bytes under the accepted pure-asset model.
6. Review claims against current source and existing tests, check local links and
   whitespace, and open a draft documentation PR for review. No runtime tests,
   schema changes, new dependencies or package release are needed for this draft.
7. At the user's request, add concrete failure timelines and a scenario inventory
   across the three contracts. Distinguish reproduced open defects, existing
   regression coverage, known limitations and hypothetical policy examples.
   Record unanswered questions without choosing their answers. Link the catalog
   from the contract index and boundary inventory; use it to scope later tests.

No new public API, immutable artifact storage, durable event replay, atomic log
persistence, garbage collection or cloud-provider guarantee is implemented here.
