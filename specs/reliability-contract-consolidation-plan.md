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
3. Separate computation keys, artifact-byte identities, durable run identities
   and current partition membership. Record saved-handle policy as unresolved.
4. Link the index from `specs/README.md`; preserve native HTTP, CLI, metadata and
   Python grammar specifications and historical implementation evidence.
5. Define bounded PR sequencing and issue ownership. Existing correctness fixes
   may proceed independently; saved-results implementation waits for its policy
   and exact API/migration/retention specification.
6. Review claims against current source and existing tests, check local links and
   whitespace, and open a draft documentation PR for review. No runtime tests,
   schema changes, new dependencies or package release are needed for this draft.

No new public API, immutable artifact storage, durable event replay, atomic log
persistence, garbage collection or cloud-provider guarantee is implemented here.
