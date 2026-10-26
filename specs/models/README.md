# Asset ownership design checks

Status: approved-design model, not implemented behavior or implementation conformance.
Owner: [current-state/execution proposal](../proposals/current-state-and-execution.md).
Model: [AssetOwnership.tla](AssetOwnership.tla). Tool/config/result identity:
[verification.json](verification.json).

## Scope

The finite abstraction has asset keys, ordered observations, captured requests, worker-owned
in-flight identities, durable completed results and current selections. An identity is a
key/revision pair in this model: revisions stand for distinct resolved input identities.
A second request for that pair subscribes without creating another computation. Automatic
pending reconciliation is derived from the latest observation; captured requests can still
need older work. Observing a newer revision does not cancel existing work.

A complete action abstracts one durable result/selection commit. Failure removes ownership
without publishing success; crash loses in-flight work but preserves requests and committed
results. Cancellation detaches a request and preserves running work needed by others.
The conservative mode prevents different versions of one key running together; concurrent
mode allows them while guarding selection order.

| Model invariant | Design obligation |
| --- | --- |
| `NoRevisionRegression` | INV-SELECT-ORDER-01: older completion cannot replace a newer selection |
| `SelectedIsCommitted` | Part of INV-COMMIT-01: a selection has a committed result |
| `NoDuplicateWork` | INV-SHARE-01: one executing owner for a matching identity |
| `CancelPreservesOthers` | INV-CANCEL-01: another request's running work survives cancellation |
| `WorkerBound` | A worker executes at most one modeled job |
| `ConservativeVersions` | Conservative admission waits rather than overlapping versions of one key |
| `TypeOK` | State stays within the declared finite domains |

## Checks and their limits

Three configurations exhaust their reachable finite states:

- [Concurrent](concurrent.cfg): one key, three revisions, two workers, two request slots.
- [Conservative](conservative.cfg): the same bounds, without concurrent versions of a key.
- [Partitioned](partitioned.cfg): two keys, two revisions each, two workers, two request slots.

[check.py](check.py) also mutates each critical guard independently and requires TLC to find
the corresponding invariant violation: late publication, duplicate owner, cancellation of
another subscriber, and unsafe version overlap in conservative mode. A syntax/tool error
cannot count as detecting a mutant; the expected invariant name must appear.
Recorded positive counts are 1,324 / 826 / 9,646 distinct states. The hashes in verification.json
bind those results to the model and configurations; regenerate it when those inputs change.

This is a safety check. Repeated failures, infinite observations or unfair admission can
prevent progress; no liveness theorem is claimed. Eventual convergence needs finite input
churn, eventual successful execution and fair admission, plus implementation tests.
The model does not check artifact bytes/path reuse, hash construction, equal data under
different observation revisions, domain membership deletion/restoration, batch mechanics,
memory/CPU, transactional substeps, actual
HTTP/worker sockets, persisted subscription storage, or multi-source revision ordering.
In particular it assumes captured inputs remain stable; the implementation's known input
read-isolation defect is not repaired by model checking. `Crash` abstracts restart at an
atomic commit seam, not every possible file/database crash point.

## Run

Java is required. The checker pins the SHA-256 of `tla2tools.jar` from the official
[TLA+ v1.7.4 release](https://github.com/tlaplus/tlaplus/releases/tag/v1.7.4):
`936a262061c914694dfd669a543be24573c45d5aa0ff20a8b96b23d01e050e88`.
It does not download or silently accept a different tool.

```bash
mkdir -p .spec-check
curl --retry 3 --max-time 60 -fsSL https://github.com/tlaplus/tlaplus/releases/download/v1.7.4/tla2tools.jar \
  -o .spec-check/tla2tools.jar
python3 specs/models/check.py --jar .spec-check/tla2tools.jar \
  --json specs/models/verification.json
```

Temporary model copies and state directories are created under this worktree's model
directory and removed even on a failed check. Each TLC process has a 512 MiB heap bound and
a 120-second timeout. Stop checks before worktree teardown; remove the downloaded tool with
the task's worktree. CI runs the same checker, including all four negative controls.
