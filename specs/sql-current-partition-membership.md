# SQL views use current partition membership (#288)

## Problem and accepted scope

SQL currently queries the latest successful artifact for every historical partition
identity of a base node. Removing a key therefore leaves that key in the SQL view.
Inspection must show current membership while durable removed-key history remains
unchanged. The separate declared-name inspection slice remains owned by root.

This worktree starts at released `52e286c` and explicitly applies prerequisite PR
#365 commits `ada0657`, `b83824f`, and `3617017` locally. It cannot merge until that
prerequisite is on current main. No public fields, selectors, flags, configuration
or key interpretation rules are added.

## Bounded implementation

1. Reuse `DecideState.expected_steps`, already registered from actual fully expanded
   prediction phases before cache selection. That manifest includes cached keys,
   all worker chunks and known `partitions_from` keys. Recovery subsets cannot
   shrink it. Add one private consuming accessor; do not reconstruct keys from
   report previews, run hashes, display-string parsing or metadata history.
2. Refactor existing explanation/status through private wrappers returning their
   unchanged public result plus the owned manifest. Public entrypoints discard
   the manifest. SQL calls the private status wrapper, so discovery/DAG loading
   and cache-aware prediction still occur exactly once. Move the existing map;
   do not duplicate it or introduce a project scan.
3. Intersect latest successful partition metadata with exact manifest identities
   before view construction. Existing stale-result semantics apply to current
   keys only. Do not execute assets, rewrite history, fetch excluded artifacts or
   add artifact copies. Use the canonical base ID to strip the partition suffix
   for its column; source filenames/directories may themselves contain brackets.
4. Unknown derived membership must not select historical rows. Record why that
   view is unavailable and use the existing missing-view/remediation error path
   (`barca get`/`run` first). Do not fail an unrelated SQL query merely because
   another node's membership is unknown.
5. A zero-key expansion has no partition artifact identity even if the existing
   planner retains its historical base-step placeholder. No partition view is
   created: there is no current result from which SQL could infer a schema. Keep
   existing no-result-view behavior and explain this in manual/site.

## Meaningful verification

- Actual CLI remove/add-key cold/warm runs at pool sizes 1, 2 and default; view
  excludes removed keys, includes current cached keys across every worker chunk,
  and includes current stale last results without reviving removed identities.
- Compare removed materialization rows before/after SQL and subsequent refresh.
- Bracketed filenames/directories and canonical IDs; coordinate declared names
  with the separate root inspection slice after integration.
- Derived cached source with known current keys, changed/uncached source with
  unknown membership, and zero-current keys. Unknown queries give honest
  remediation while unrelated views remain queryable; no user imports during
  inspection for supported static/derived-cached inputs.
- Existing SQL/status/planning/artifact tests, relevant workspace checks, pinned
  formatting/type checks, and affected embedded/manual/site examples.

## Compatibility and residual limits

SQL becomes a view of the currently selected partition identities, not every key
that ever ran. Historical records remain accessible through existing history/run
inspection. Known current keys can still show stale successful values until a
refresh, matching established SQL behavior. Unknown derived membership and empty
membership have no result view. Existing expression-partition evaluation and
remote-state/history side effects are unchanged; no new imports or data movement
are introduced by membership filtering.
