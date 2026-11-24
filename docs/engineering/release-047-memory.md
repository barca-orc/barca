# 0.47 release memory headroom

The release CI incremental gate exceeded its unchanged 19,531 KiB budget twice,
including its own retry (19,888 and 19,652 KiB). Local tracing showed about 15–16 MiB
of file-backed resident pages and 3.1 MiB anonymous memory during warm execution;
the command future was only 16 KiB. This points to executable footprint rather than
whole-domain per-key state in this warm path.

Compile only `barca` (the CLI) and `barca-run` with `opt-level = "z"`. Other crates
keep the existing `"s"` level. Both fast and release profiles inherit these package
overrides. Worker parallelism, validations and performance budgets are unchanged.

Measured on 2026-11-24 with the fast profile, matching Python helpers, and 5,000
keys in the existing incremental harness, before building the embedded UI. Baseline is release head 5f378a82;
comparison uses only the two package overrides. The harness verifies three
executed steps/calls/identities per changed-key run and one step for unchanged work.

- Baseline: 10 one-key updates plus unchanged, median peak 18,936 KiB,
  maximum 19,328 KiB; median CPU 11.097 ms; median wall 53 ms.
- Compact CLI and run: 20 one-key updates plus unchanged, median peak 18,372 KiB,
  maximum 18,836 KiB; median CPU 10.766 ms; median wall 50 ms.
- Full performance gate: trivial maximum 18,044 KiB, incremental maximum
  18,616 KiB, history lookup median 1.3 ms. All existing budgets pass.

These are local measurements, not a guarantee for every runner. CI rechecks the
same unchanged budgets. [Raw results](release-047-memory.json) retain cold/proof
and warm rows, including the larger cold bootstrap still tracked separately by #655.

Reproduce with `BARCA_BIN=.venv/bin/barca KEYS=5000 REPEATS=20
.venv/bin/python benchmarks/incremental_dag/bench.py`, then
`BARCA_BIN=.venv/bin/barca .venv/bin/python scripts/perf-gate.py`.

Final fast-profile checks with the production UI embedded also pass: trivial peak
17,868 KiB, incremental peak 18,548 KiB and history lookup median 1.5 ms.
Correctness checks pass: 1,705 Rust tests, 560 selected Python tests, all local
check-fast gates, and all 106 Playwright tests (79 flat, 25 grouped, 2 auth).

The optimized release-profile build with the embedded UI also passes the same
performance gate: trivial peak 16,324 KiB, incremental peak 16,620 KiB and history
lookup median 1.3 ms.
