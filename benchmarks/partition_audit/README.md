# Sensor and partition audit, 2026-10-25

Audit baseline: published PyPI `barca==0.33.0`, CPython 3.13.16, Linux x86-64,
Ryzen AI MAX+ 395, pool four for measurements. Exact installed dependencies,
affinity and wheel binary SHA-256 are in [environment.json](environment.json).
Measurements ran sequentially, without concurrent builds or tests. CPU affinity
was recorded but not pinned for this audit. These are Barca checks, not a new
cross-framework comparison. Rust RSS excludes Python; sampled process-tree RSS
sums live RSS and can double-count shared pages or miss short-lived processes.

## Findings

| Finding | Evidence on released wheel | Tracking |
|---|---|---|
| Historical partition selection returns stale data on next hit | Source `1 -> 2 -> 1 -> 1` returns `2 -> 4 -> 2 -> 4`, expected final `2`; unpartitioned control passes | [#657](https://github.com/barca-orc/barca/issues/657) |
| Overlapping identical assets execute twice | Two requests, same node and run hash, two calls; explicit tasks correctly execute twice | [#654](https://github.com/barca-orc/barca/issues/654) |
| Full bootstrap retains whole-domain work | 200k keys: 68.067 s, 2,880,112 KiB native peak, 66.901 s native CPU, 400,001 logical steps in 401 Python calls | [#655](https://github.com/barca-orc/barca/issues/655) |
| Inspection invalidation remains expensive | Three-node/200k-key fixture: cold `/state` 3.008 s, ~1.16 GiB native peak; five post-run reads 2.67–2.72 s, retained RSS ~1.22 GiB | [#656](https://github.com/barca-orc/barca/issues/656) |

The bootstrap numbers are single samples, with individual per-key artifacts.
Native and worker CPU overlap: do not add them to derive wall time. This is not
proof of a new regression relative to an older release. Inspection has four
reads per route and five post-run samples; it is not a long-duration soak or a
measurement of the authenticated Utz deployment. Warm requests hide the cold
inspection cost: `/assets` takes <1 ms and cached `/state` about 1 ms.
The concurrent health probe returned in 0.8 ms, but does not establish a tail
latency guarantee under sustained concurrent requests.

## What passed

The released wheel passed 158 focused sensor, partition, batching, cutoff,
planning and scheduling tests. Another 88 tests passed covering the initial
new audit checks, incremental persistence/recovery, server ledger, run queue,
shared capacity, timeout and local-directory remote storage. These are selected
suites, not a rerun of every emulator/cloud test or the entire Python suite.

The changing-domain oracle initially passed twenty transitions in each of three
batch modes (`None`, `3`, `all`): additions/deletions/changes return exactly the
current keys and values, executing only uncached key values. Extending it to an
empty domain and restoration exposed #657 on the following unchanged read.
The permanent regression also checks historical rows survive key deletion.
A partial batch failure commits successful keys, runs their downstream work,
and a second request retries only the failed key. Explicit concurrent tasks
remain separate invocations. Each CLI invocation is a fresh process, so the
changing-domain cases also exercise durable evidence across process restarts.

## Released performance

[18 incremental records](released-incremental.jsonl) include bootstrap, the next
unchanged/proof run, three updates and an unchanged run at 1k/50k/200k keys.
All eight large warm samples satisfy <20,000,000 native bytes and <20 ms native
CPU. At 200k, one-key runs register exactly three identities and make three
function calls; unchanged registers one. Small targets return all references
and are measured separately; their native CPU exceeds the single-chain budget.
The immediately following proof run at 200k is 0.435 s / 46.8 MiB native peak;
it is distinct from the 68 s full bootstrap.

[Process samples](released-resources.jsonl):

| 200k-key operation | Wall | Rust peak | Rust CPU | Peak processes |
|---|---:|---:|---:|---:|
| Unchanged | 0.291 s | 13.5 MiB | 11.9 ms | 2 |
| 17 dispersed keys | 0.323 s | 14.6 MiB | 22.7 ms | 3 |
| Cold selected key | 0.285 s | 13.8 MiB | 51.6 ms | 3 |

Process sampling is every 2 ms. Ordinary no-store runs assert at most five
processes (coordinator plus configured pool); no per-partition process creation.
Nested `parallel()` and remote transfer helpers require separate budgets.
[Inspection records](released-inspection.jsonl) include server native RSS/HWM.

## Reproduce

In an owned worktree, install the released wheel and test dependencies:

```bash
uv venv --python 3.13 .venv
uv pip install --python .venv/bin/python 'barca[test]==0.33.0'
export PATH="$PWD/.venv/bin:$PATH"
export BARCA_BIN="$PWD/.venv/bin/barca"
mkdir -p audit-runtime
KEYS=1000,50000,200000 REPEATS=3 BARCA_POOL_SIZE=4 \
  PROJECTS_DIR="$PWD/audit-runtime/performance" \
  .venv/bin/python benchmarks/incremental_dag/bench.py
.venv/bin/python benchmarks/partition_audit/read_routes.py \
  audit-runtime/performance/keys-200000
.venv/bin/python benchmarks/partition_audit/resources.py \
  audit-runtime/performance/keys-200000 -- get scored pipeline.py --json
.venv/bin/python benchmarks/partition_audit/check_overlap.py
.venv/bin/python -m pytest -q python/tests/test_partition_audit.py
```

The overlap script intentionally fails until #654 is fixed. It checks real
server execution and shuts it down even when the assertion fails. The historical
selection regressions fail on the released wheel and must pass on a fixed build.
Use a fresh PROJECTS_DIR for the incremental controller; resetting versions in a
retained history can select older cached rows that its latest-row output checker
is not designed to inspect. Reuse retained projects through `resources.py` for
unchanged/selected operations, or increment their existing versions explicitly.
Remove all generated artifacts and environments with the worktree after reaping
processes; 200k seed artifacts occupy several GB of disk.

## Historical-selection fix

The fix retains only historical run-hash exceptions in the generation-scoped
freshness proof, capped at 256 KiB per logical node. When it loads a fresh
partition reference, it selects that hash rather than blindly choosing the last
materialization row. Entries are cleared when normal execution/cache selection
returns to the latest row. An oversized exception set drops that node's proof
and uses ordinary validation. Proof namespace v3 ignores existing v2 evidence;
there is no worker-protocol or metadata-schema change. Discarded proofs are explicitly
deleted even when cache-only work leaves the metadata generation unchanged.

Validation on the fixed execution code: **1,176 Rust tests, 169 focused Python
tests, workspace Clippy with warnings denied, and Ruff check/format pass**. The
Python pass includes eleven new audit regressions. The 3,000-key historical
reversion test asserts the oversized proof is actually dropped and the results
remain correct. Existing recovery and remote-ledger suites (88 tests) were run
on the released baseline, not repeated in full after the fix. No claim is made
that the entire Python or cloud/emulator suite was rerun locally.

[Fixed measurements](fixed-incremental.jsonl) include the one-time proof upgrade:
50k keys takes 2.759 s / 373 MiB native peak; 200k takes 10.107 s / 1.32 GiB.
That full-validation path remains expensive and belongs to #655. Post-upgrade
50k warm samples remain below 20 MB / 20 ms native CPU. The first 200k update
fails the strict CPU gate at **22.147 ms**, with 0.741 s wall and 642.6 ms worker
CPU. It retains only three identities and uses 13.8 MiB native memory.

We retained that failure and ran [five follow-up updates](fixed-followup.jsonl):
12.7–17.3 ms native CPU, 13.6–14.1 MiB native peak, median 0.289 s wall; unchanged
is 0.257 s / 12.5 ms native CPU. These do not erase the earlier failed budget
sample or prove its cause. Affinity was not pinned; do not claim a universal
20 ms maximum from this run. The fixed local binary was built without bundled
UI assets; the published-wheel baseline includes UI. Compare execution counts
and correctness directly; small footprint differences are not a controlled
wheel-to-wheel regression comparison.

The fix closes #657 only. Matching in-flight work (#654), full-domain coordinator
state (#655), and inspection invalidation (#656) remain open. The next audit
needs a long server soak, realistic large-history and remote-ledger performance,
partial-overlap partition requests with cancellation/failure, and controlled
cold/refresh measurements after those architectural fixes. The three-node
fixture and selected tests do not establish correctness of every graph shape.

The final source revision also restricts historical lookup to requested keys
and explicitly deletes discarded same-generation proofs. Its [build manifest](final-build.json)
and [four final warm records](final-warm.jsonl) identify the tested binary.
Three final 200k one-key updates use 13.1–14.3 ms native CPU and <=13.93 MiB
native peak; unchanged uses 11.9 ms. All four enforce <20,000,000 native bytes
and <20 ms CPU. Final-binary focused Python tests: 169 passed in 68.57 s;
final-source Rust workspace: 1,176 passed; Clippy and Ruff pass. The earlier
22.1 ms failure and proof-upgrade costs remain relevant limitations above.
