# Engineering notes

These documents are non-normative. Approved behavior and interface owners live in
[specs](../../specs/README.md); implementation plans, rationale and discussion state cannot
change those contracts.

- [Pre-merge specification audit](spec-audit.md): separate unconsidered boundaries, implementation disagreements and spec-quality blockers.
- [Architecture map and cleanup sequence](architecture-cleanup.md): inspected owners and staged refactors.
- [Requirements discussion handoff](requirements-handoff.md): unanswered proposals and remaining decisions.
- [User API rationale](user-api-decisions.md): reasons behind declaration choices.
- [DuckDB lifetime investigation](history/duckdb-step-isolation.md): retained empirical evidence and rejected isolation alternatives.
- [DuckDB reproduction](reproductions/duckdb_step_isolation.py): executable investigation, not conformance.

## Removed completed implementation plans

Current behavior is recorded in specs and checked by its linked tests. The following completed
plans were deleted rather than moved into a new archive; their original changes retain the
historical reasoning and verification logs. Nine other superseded plans were already removed
in the earlier specification cleanup commits on this branch.

- `complete-upstream-cache-boundary`: [original change](https://github.com/barca-orc/barca/pull/365).
- `explicit-project-imports`: [original change](https://github.com/barca-orc/barca/pull/371).
- `helper-import-consistency`: [original change](https://github.com/barca-orc/barca/pull/358).
- `idle-server-telemetry`: [original change](https://github.com/barca-orc/barca/pull/356).
- `inspection-node-names`: [original change](https://github.com/barca-orc/barca/pull/373).
- `lexical-unused-inputs`: [original change](https://github.com/barca-orc/barca/pull/349).
- `serve-load-isolation`: [original change](https://github.com/barca-orc/barca/pull/361).
- `sql-current-partition-membership`: [original change](https://github.com/barca-orc/barca/pull/378).
- `staged-file-finalization`: [original change](https://github.com/barca-orc/barca/pull/380).
- `static-barca-bindings`: [original change](https://github.com/barca-orc/barca/pull/355).
