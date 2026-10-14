# Current-behavior documentation audit (#294, S01)

Scope is accuracy of help/manual/site/agent guidance against current main, not
changes to runtime policy, storage contracts or public APIs. Future freshness and
partition result designs remain proposals. SQL installation is owned by PR352;
partition pool/preview ordering by PR354; remote preflight permissions by PR348.
Those changes are excluded from this patch.

## Bounded corrections and verification

- Correct the freshness table and Manual example comment; sensors accept Always,
  and a scheduled sensor does not trigger its consumers. Verify Manual cold/warm,
  code edits, Always sensor execution and a real scheduled sensor with a downstream
  asset that remains unmaterialized.
- Correct partition final_output wording: the first key's value is returned, while
  all requested keys are materialized/checked. Existing inline-key add/remove
  examples are now accurate after #305; execute their current examples.
- Preserve accurate parallel-from-assets/typed-results documentation after #314;
  execute the manual branch example, do not reintroduce older restrictions.
- Explain state-off artifact uploads without cross-machine cache discovery;
  qualify identical-history corruption-check optimization, and keep current
  non-destructive compatibility recovery from #350. Verify two local projects
  sharing one file store with state off, plus existing recovery tests.
- Document existing transfer environment variables and exit-three validation.
  Correct SQL side effects (shared-history synchronization and explicit COPY),
  scalar-list JSON views and --env hints; leave install recipe to PR352.
- Keep root and embedded skill identical; qualify blanket no-write/no-import
  claims for shared state and evaluated partition expressions. Fix overview's
  command table and verify every embedded topic is linked from overview/examples.
- Remove the stale partition-cache limitation and distinguish run-hash addressing
  from content-byte addressing in contributor guidance. Contributor recipes already included the test extra, but now also activate the
  venv and use maturin --uv so the installed tools and uv-created environment work
  together. Verify the real editable install using the cached development profile.
- Link actual latest release notes through v0.20.1 and label the older changelog
  section historical. Mark accepted-baseline RFC pages as historical design
  records, with links to current references and explicit freshness deviations;
  do not rewrite their original proposals into fictional implementations.

Run affected actual CLI examples using this worktree's binary/package, existing
manual/contract/recovery tests, Rust CLI docs/contract tests and site link/build
checks. Record confirmed/corrected claims and runtime limitations separately.

## Original and inherited audit checklist

| Claim from #294 / #229 | Current evidence and disposition |
|---|---|
| Inline partition-key edits recompute only added keys | Already corrected after #305; manual partition add/remove examples execute successfully. No duplicated patch. |
| Partition target returns the whole result | Corrected: CLI returns the first lexical key while materializing all requested keys; cold/warm three-key example verified. Selective result API remains #287/#57. |
| Always/Manual runtime barriers | Corrected manual table/comment. Real Manual cold/warm/code-edit runs verify no barrier; site already described this accurately. |
| Sensor rejects Always | Corrected manual and site signature; two consecutive CLI sensor runs accepted and executed. |
| Scheduled sensor triggers consumers | Explicitly document that it does not; actual server tick leaves downstream body unexecuted. |
| parallel only runs inside tasks | Already corrected after #314; actual asset fan-out returns both results and cached repeat executes no steps. Existing manual branch-return examples pass. |
| State-off shares cache results between machines | Corrected uploads/discovery distinction; two independent projects each execute once against the same file store, then reuse their own local rows. |
| Damaged identical shared-history bypass | Qualified using replace_db's SameAsLocal boundary; existing actual CLI corruption, salvage and compatibility recovery tests pass. Schema mismatch never warrants reset. |
| Missing transfer variables / validation code | Document both existing variables; actual zero-value CLI cases exit 3 before metadata directories. |
| SQL side effects / scalar views / environment hint | Shared-history SQL creates local metadata but no new run; scalar list has json column; explicit COPY creates CSV. Document appending the same --env to the current hint. Install recipe remains PR352. |
| Overview table | Move docs row back inside the table; all topic commands and reachability validated by CLI docs tests. |
| CLAUDE addressing / cache limitation | Clarify run-hash addressing; replace obsolete uncached-partitions example with current first-key return limitation. |
| Contributor recipe | Activate venv/use --uv; actual editable install with all test dependencies succeeds. Release profile is a build option, not newly recompiled for this docs-only audit. |
| Root/embedded skill | Copies remain identical; existing parity/frontmatter tests pass. Qualify source-expression imports and shared-history writes. |
| Changelog currency | Link verified published v0.20.1/v0.20.0 release notes; label older formerly-Unreleased notes historical. No release pipeline change. |
| RFC vs implementation | Add historical design-record banners to six accepted-baseline RFC pages; explicit freshness differences and proposal link on RFC0001. Preserve original proposals/amendments. |

## Verification and separate limitations

113 actual CLI Python checks pass across new audit evidence, manual examples, CLI
contract, SQL, scheduling and state validation. After the final editable install,
60 manual/contract checks pass again and installed SQL help matches its snapshot.
All 53 Rust CLI checks, strict CLI Clippy, Ruff, formatting and whitespace checks
pass. The documentation site builds all 51 pages. The actual development-profile
maturin --uv editable install succeeds with the declared test extra.

No product behavior or API is changed. Existing limitations are described rather
than endorsed as new policy: evaluated partition expressions import source while
loading, optimistic inspections may synchronize metadata, partition final_output
is one key, and SQL's missing-view hint omits the selected environment. During the
asset parallel example the outputs are correct but steps_executed reports one;
branch-inclusive accounting differs from task fan-out. Report this separately for
runtime ownership rather than alter it in a documentation patch.
