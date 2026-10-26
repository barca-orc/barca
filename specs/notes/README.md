# Historical implementation notes

These notes retain focused reasoning and reproduction evidence from individual changes.
Their imperative wording, measurements and source paths describe those changes at the
recorded time. They are not contracts, current status reports, or a second roadmap.
Use [the specification index](../README.md) to find current behavior and approved proposals.

| Historical note | Current owner |
| --- | --- |
| [complete-upstream-cache-boundary](complete-upstream-cache-boundary.md) | [cache decisions](../cache-decisions.md) |
| [duckdb-step-isolation](duckdb-step-isolation.md) | [Python runtime](../python-runtime.md) |
| [explicit-project-imports](explicit-project-imports.md) | [cache provenance](../cache-decisions.md) and [declarations](../user-api.allium) |
| [helper-import-consistency](helper-import-consistency.md) | [cache provenance](../cache-decisions.md) and [Python runtime](../python-runtime.md) |
| [idle-server-telemetry](idle-server-telemetry.md) | [telemetry manual](../../crates/barca-cli/docs/telemetry.md) |
| [inspection-node-names](inspection-node-names.md) | [inspection](../inspection.md) and [wire schema](../server-api.openapi.yaml) |
| [lexical-unused-inputs](lexical-unused-inputs.md) | [cache provenance](../cache-decisions.md) |
| [serve-load-isolation](serve-load-isolation.md) | [execution](../execution.md) and [inspection](../inspection.md) |
| [sql-current-partition-membership](sql-current-partition-membership.md) | [current membership](../freshness.md) and [SQL manual](../../crates/barca-cli/docs/sql.md) |
| [staged-file-finalization](staged-file-finalization.md) | [upload receipts](../artifact-upload-receipts.md) |
| [static-barca-bindings](static-barca-bindings.md) | [declaration model](../user-api.allium) and [cache provenance](../cache-decisions.md) |

## Removed superseded plans

The stale plans below were removed in the specification cleanup. Their PRs retain the
historical evidence; the live rules are already covered by current owners. There is no
archive directory of competing contracts.

| Former note | Historical change | Current owner |
| --- | --- | --- |
| Committed progress recorder | [#360](https://github.com/barca-orc/barca/pull/360) | [Cache decisions](../cache-decisions.md) |
| Atomic terminal ledger | [#362](https://github.com/barca-orc/barca/pull/362) | [Cache decisions](../cache-decisions.md) |
| Partition planning first slice | [#354](https://github.com/barca-orc/barca/pull/354) | [Cache decisions](../cache-decisions.md), [freshness](../freshness.md) |
| Emulator crash test plan | [#367](https://github.com/barca-orc/barca/pull/367) | [Upload receipts](../artifact-upload-receipts.md), existing remote/crash suites |
| Linux packaging plan | [#372](https://github.com/barca-orc/barca/pull/372) | [Release workflow](../../.github/workflows/release.yml) |
| Published artifact verification plan | [#379](https://github.com/barca-orc/barca/pull/379) | [Release workflow](../../.github/workflows/release.yml), [package smoke check](../../scripts/smoke-release-package.py) |
| Process test readiness plan | [#377](https://github.com/barca-orc/barca/pull/377) | [Execution](../execution.md), helper lifecycle tests |
| Storage override plan | [#353](https://github.com/barca-orc/barca/pull/353) | [Execution](../execution.md#process-storage-overrides), [remote manual](../../crates/barca-cli/docs/remote.md) |
| Remote startup preflight plan | [#348](https://github.com/barca-orc/barca/pull/348) | [Execution code](../../crates/barca-core/src/execution.rs), [preflight tests](../../python/tests/test_remote_preflight.py) |

The old preflight plan's unconditional-before-workers and shared-history timing claims are
not retained as requirements: the execution path can defer store checking until a fetch.
The old storage plan's `BARCA_STATE=off` is removed behavior. Do not revive either through a
historical note. Backups, pending uploads and server-ledger sharing have their own owners.
