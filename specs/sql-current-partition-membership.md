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

## Verification evidence

Implementation uses a private owned `ExpandedMembership` alias and consuming
`DecideState` accessor. Private explanation/status wrappers return that existing
map alongside unchanged public results; SQL performs an exact-ID intersection.
Known-base prefix stripping fixes partition labels in bracketed source paths.

All 11 new actual cases pass: remove/add/stale rows at pools 1/2/default and both
plain/bracketed paths, derived cached-known versus changed-unknown sources, zero
current keys, and excluded remote downloads. Four distinct cases fail against the
previous installed CLI before this fix (historical membership, unknown derived,
zero keys, and remote historical fetching). The remote case instruments the real
SQL child using process-local fsspec memory objects: exactly three current objects
download and neither removed URI does; all materialization rows stay unchanged.
This proves the selection/download boundary, not a cloud provider acceptance test.

On the released base plus explicit #365 prerequisite, all 803 Rust workspace tests,
strict all-target Clippy, and 146 SQL/status/prediction/pool/remote-inspection tests
pass. Eight existing cloud emulator cases skip because no emulator is running;
the new actual remote-helper regression does not skip. All 64 real manual/CLI
contract examples pass. Website builds 51 pages; help snapshot changes are only
the two lines describing current/unknown/zero partition membership. Pinned Ruff,
formatting, version sync and lock checks pass; type checking reports no errors
(with existing dependency/environment warnings). No JSON/TS fields or history
writes are introduced. Integration must remove the local prerequisite copies and
rerun relevant checks after #365 lands on current main.

After #365 merged at `e110f80`, the branch was rebased onto authoritative main
and all local prerequisite copies dropped. Only this membership plan and change
remain. The rebuilt package passed 123 actual SQL/membership/API/manual/contract
tests; the full Rust workspace passed 812 tests and strict all-target Clippy.
Formatting, pinned Ruff, lockfile and whitespace checks also passed. The prior
remote-helper proof remains part of the 11 membership regressions and ran again.

## Naming and partial-loader integration plan

After main #361 and #373, status_from_dag is the existing public entrypoint for
serve's validated source snapshot. Preserve its signature and StatusResult return,
along with declared StatusNode names and generated descriptions. Move the shared
status/prediction body into a private status_from_dag_with_membership helper;
public status_from_dag discards the private membership, while SQL's private
status_with_membership loads one DAG then delegates to the same helper. Do not
reload a graph provided by serve, reinterpret keys, duplicate prediction or add
public fields. Combine current-key documentation with declared SQL view names.
Verify original load-isolation inspection/scheduling, existing naming cases and
an actual declared-name partition view after removing/adding keys, preserving
canonical row IDs and removed history. Publish only after readiness #377 merges.

The shared-body rebase onto naming main `f4c4ab6` preserves the public validated
status entrypoint and the generated name descriptions without a generated-file
diff. Root independently reviewed the resolved status/SQL wrapper delta and
found no blocker. The new declared-name test uses the existing explicit name
as the canonical identity (`current_view[k=...]`), not file/function IDs;
unnamed bracketed-path identities remain covered separately. All 28 current
membership/naming/load-isolation cases pass, and the existing SQL cases pass.
The full 832-test Rust workspace and strict Clippy passed before the readiness
prerequisite merge. Final publication/reverification follows #377 on latest main.
