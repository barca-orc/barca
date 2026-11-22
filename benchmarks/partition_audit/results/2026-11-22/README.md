# Compact inspection investigation (#656)

Current-main baseline: `a8bf85e7`, fast profile, pool 2, Linux x86-64, Python 3.14.8.
The 1k and 5k records use the existing three-node incremental DAG fixture. Native
RSS/VmHWM are server-only, not Python worker or process-tree memory. Ambient machine load
was substantial (the 200k record records load averages); these are local short samples,
not deployment validation or a leak test. All projects/files are real and seeded with get.

The 5k main baseline had cold state 106–117 ms and peak 57,184–60,844 KiB. `/nodes?sample=0`
raised peak to 85,472 KiB. Post-unchanged-run state was 80 ms; settled cached reads were
0.8–1.4 ms. `main-5000-profile.txt` attributes 206 ms to full prediction versus 8 ms
history and 64 ms shapes in a separate CLI sample. These are separate samples, not a
subtraction of times from one request. A 48,828 KiB local budget failed on cold state.

`prototype-200000-failed.jsonl` retains the first compact predictor, before removing
uncounted per-key history retention and stabilizing availability evidence across read views.
It failed the 97,656 KiB combined budget: roughly 248,340 KiB retained/peak.

`history-fixed-200000-failed-budget.jsonl` includes bounded uncounted history and paged
availability. Cold state: 3.337 s, 74,152 KiB peak. Settled retained: 49,136 KiB.
Most post-run requests: 83–120 ms; fixed-expiry rescans can hit the server's one-second
background wait. Cached responses: 0.8–2.7 ms. During a deliberately held real sensor
poll, state took 69 ms. Health: 771 samples, maximum 173 ms, no errors. The combined
server/served-run peak of 102,752 KiB **failed** the proposed 97,656 KiB budget.
The default acceptance probe retains the approved 100 MB bound; it is intentionally
not green yet. Selecting a larger investigative limit does not change acceptance.

Reproduction (in a built worktree; binary and helpers must match):

```bash
PATH="$PWD/.venv/bin:$PATH" BARCA_BIN="$PWD/.venv/bin/barca" BARCA_POOL_SIZE=2 \
  KEYS=200000 PROJECT_DIR="$PWD/target-local/inspection-200000" \
  .venv/bin/python benchmarks/partition_audit/compact_inspection.py
```

Use a fresh owned directory. Repeat separately with KEYS=5000 and KEYS=50000; seed memory
is not inspection memory and remains unbounded. To probe an existing seeded directory,
use `recheck_inspection.py` with PROJECT_DIR, BARCA_BIN, EXPECT_KEYS and ACTIVE_CHECK=1.
That active check requires the delayed-sensor fixture produced by compact_inspection.py.
The harness checks every selected-file result count, not only HTTP success, and reaps its
server and active get even on failure. JSONL measurements are printed before budget failure.

Limits: remote stores/ledgers and invalid or unsupported proofs still use full prediction.
SQL key listings explicitly expand membership. Counted-key history, long failure/change
history, full refresh, wide graphs and long-duration retention remain separate scaling work.
Missing files are re-observed within the existing 30-second polling window plus computation;
required reads do not use the five-second local availability memo. #656 remains open.

The final post-run-node probe (`final-post-nodes-200000-failed.jsonl`, identified by
`final-environment.json`) found state 56–61 ms and nodes 90–125 ms after unchanged runs,
but cold state 11.265 s and combined peak 135,024 KiB failed both the 10 s and investigative
128 MiB limits. Health max was 27.5 ms, without errors. A follow-up cold-only profile on
the same binary/seed, after local checks finished, measured 2.579 s / 85,284 KiB peak.
Its trace attributes 1.47 s to selected-artifact validation and 42 ms to status history;
the separate age aggregation is outside those spans. Both samples are retained; the
faster cold profile does not invalidate the failed loaded sample or close the budget gap.
