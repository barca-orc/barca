# Writer-version trigger performance investigation (PR #1113)

Status: a measured allocation improvement, **not proof that the merge-queue performance
failure is resolved**. No performance limit, gate, retry policy or cache treatment changed.

## Failure and comparable builds

Queue commit `8f906157dab4d80718ed77d64d2dfaeb5da2533a` and PR head
`c76a4c2bc686edc360d29fe5be2ebdd34411da3b` have the same tree
(`3cfe300dba6f168779ed5ace1d7e5a37d3630393`). The queue baseline is
`63a5ed347d3bbdcbcab634b2736f7800823a37c5`. Depot job `41gmq8glfw`, workflow
`tl1pm7tw4h`, failed the 5,000-key incremental memory check at 19,844 KiB and 20,512 KiB
on its built-in retry, against the unchanged 19,531 KiB limit. CPU was about 10 ms against
40 ms. Trivial memory (19,232 KiB) and history (1.4 ms) passed.

The actual queue workflow builds with `maturin build --profile fast`, not `--release`.
All completed comparison builds here used `maturin develop --uv --profile fast --extras test`
in the same worktree and private `target-local`, with four build jobs, two Rust test threads
and `BARCA_POOL_SIZE=2`. An initially requested release build was stopped before completion;
no release-vs-fast measurements enter these results. Existing locked dependency versions
match between baseline and PR; the PR adds semver and the PEP 508 parser/dependencies.

Machine: Ryzen AI MAX+ 395, 32 logical CPUs, Fedora Linux 7.2.8, x86-64, rustc 1.100.0
(`0ef1bc386`, LLVM 23.1.5), Python 3.14.8. Builds used `direnv exec
/home/mukulram/Personal`; no custom RUSTFLAGS or CPU affinity. Host load ranged roughly
9–16 during the investigation; other worktrees were active. The final cache experiments
overlapped this worktree's bounded clippy/Rust test run. This is not Depot's Ubuntu runner.

SHA-256 identities (copied binaries remain beside the same venv Python):

| Build | SHA-256 |
|---|---|
| Baseline | `6188ea6624616f7c955ae3b273904191c3e81c7e0129a7e5da5daebf17be28dc` |
| Original PR | `735edb290de4c448aefaa73dbba76e281de757eb08614be62ea52ae591e3b199` |
| Trigger on/off diagnostic | `1220c434f054ffd5e90faf9418c86d54611350fcafc590294f2114310b25414e` |
| Compact-trigger diagnostic | `16e36e1ff484f2cb384810f7c0ced0c3324b640ffe94b4d8d1d4883f4a27b333` |
| Combined diagnostic | `bc9bac00dbfe9b088c657c14457d6192d1ecad54b6cc5f56ff574a1c10f24f26` |
| Optimized production build | `e8227eac92876842a27e41c38a1f7db32fb85010978b7161eed082d7533cf089` |

Installed and target binary hashes matched. The final binary contains
`WHERE value != excluded.value` and contains no diagnostic toggle names. The doctor parser,
sensor warning/deduplication paths and other original PR changes remain intact.

## Measurement method

The first red-capable command was the existing gate:

```bash
BARCA_BIN="$PWD/.venv/bin/barca-pr-fast" BARCA_POOL_SIZE=2 \
  timeout 180 .venv/bin/python scripts/perf-gate.py
```

It failed incremental memory at 24,512 and 24,748 KiB. The same command with the baseline
failed at 22,212 and 21,968 KiB. Both also failed trivial memory locally; CPU and history
passed. These local absolute failures differ from the CI failure's scope.

Longer samples use `benchmarks/incremental_dag/bench.py` unchanged for pipeline generation,
execution, work-count assertions and reported native resources (`KEYS=5000 REPEATS=30`).
Each independent variant seeds its own project, performs the proof run, then changes `r0`
30 times, then runs unchanged. Controlled comparisons alternate variant order on each
iteration. The initial `paired` experiment alone shares a baseline-seeded project between
baseline and PR, so separate native-seeded cohorts and later independent projects check that
migration does not explain the observation. Early baseline/PR runs use saved matching Python
helpers. Later controls share PR helpers; the benchmark uses dictionary sensor values, not
the changed explicit-signal warning path.

For diagnostics, a copy of the harness also samples `/proc/<pid>/smaps_rollup` when its 1 ms
RSS sampler observes a new peak, and records `/proc/<pid>/exe` and the native trace peak.
The observed executable was barca, not a pre-exec Python process. Anonymous memory and file
PSS are sampled observations, not independent exact peaks. Every one-key run still asserts
three steps, three calls and three identities; unchanged runs assert one step and identity.
Diagnostic collection continues after a budget exceedance to retain distributions; it does
not report the collection as a passing gate. The real gate retains its original assertions.

[All 1,258 captured samples and the three gate results](../../benchmarks/incremental_dag/writer-version-evidence.json)
include seed/proof/unchanged samples and failures, in capture order. `sample_fields` defines
the arrays. Null allocation fields identify runs before that instrumentation was added.

## Attribution and change

Ranked hypotheses were TEMP-trigger allocations, new dependency/code footprint, and a
baseline/environment failure. Disabling only trigger installation in one diagnostic binary
isolates that path without changing executable layout. Thirty alternating one-key samples
(`smaps`) had median anonymous memory 3,624 KiB with triggers versus 3,300 KiB without.
Native CPU rose by roughly 3–4 ms, still under its budget.

A second diagnostic binary selected original, compact, optimized installation or disabled
triggers at runtime. The combined experiment (`stream-control`, 30 samples per mode) found:

| Mode | Median anonymous KiB | Median sampled RSS KiB | Median CPU ms |
|---|---:|---:|---:|
| Original triggers | 3,624 | 19,568 | 16.014 |
| Compact trigger only | 3,556 | 19,436 | 14.857 |
| Compact + missing-trigger installation | 3,512 | 19,402 | 14.983 |
| No triggers (diagnostic only) | 3,312 | 19,256 | 12.707 |

The shipped change keeps TEMP triggers and their transaction semantics, but:

- Puts the version predicate on the upsert instead of a separate `WHEN SELECT` lookup.
- Prepares one trigger statement at a time. libsql 0.10.0's `execute_batch` prepares each
  remaining SQL tail; its `prepare` converts that tail to a String. Concatenating all triggers
  therefore repeatedly copies the suffix and needs larger temporary buffers.
- Selects only missing table/event triggers. Repeated installation avoids preparing no-op
  DDL; a partial installation or newly added table is still completed.

This establishes about 112 KiB less anonymous memory in a same-binary control. The optimized
production build measured 3,516 KiB versus 3,620 KiB for the original in the final comparison.
The new dependency/code contribution was inspected but not removed: the original PR binary
has about 1.8 MB more text and 249 KB more data than baseline. That static size is not a
measurement of its resident contribution. Replacing the standards-compliant doctor parser
without attribution would risk its contract.

## Why the absolute RSS result is not settled

The same baseline hash first failed all 30 samples at a median 22,638 KiB, then passed all
30 in a later comparison at 18,298 KiB. Its anonymous memory remained around 3,040 KiB;
the difference was file-backed memory. Original PR and diagnostic binaries also showed this
variation. Comparing two freshly linked binaries alone would misattribute it to code.

A controlled experiment copies identical executable bytes before **every** invocation, using
1 MiB buffered writes, flushes and `fdatasync`s the destination, and alternates between:

- Executing the freshly copied file.
- Applying `os.posix_fadvise(fd, 0, 0, os.POSIX_FADV_DONTNEED)` to that same copied executable
  before execution. This is a diagnostic intervention on an owned binary, never a gate change.

Twenty one-key samples per mode, with independent seeded projects, produced:

| Build | Fresh-copy median KiB; memory failures | Evicted-file median KiB; memory failures | Anonymous KiB, fresh / evicted |
|---|---|---|---|
| Baseline | 22,656; 20/20 | 18,282; 0/20 | 3,048 / 3,040 |
| Original PR | 24,424; 20/20 | 19,454; 9/20 | 3,622 / 3,620 |
| Optimized | 24,716; 20/20 | 19,290; 3/20 | 3,512 / 3,516 |

These are memory-failure counts, not passing-gate counts. The cache-control cohorts also
contain CPU outliers above 40 ms (up to 74.652 ms), and the disabled-trigger mode in
`stream-control` has one 318.464 ms CPU outlier. They are retained in the raw data and in
the reported medians; no outlier was discarded.

This proves executable page-cache treatment can move the reported footprint by several MiB
without changing its bytes or allocation behavior on this host. It does not prove which
kernel mechanism or cache state caused the Depot failure. Reading a previously evicted file
back into cache also does not reproduce the full fresh-copy effect (`cache-control`).

The final unmodified gate **still fails locally**: incremental 24,584 / 24,788 KiB, trivial
24,136 / 24,244 KiB; history 1.3 ms passes. The optimization reduces measured allocations but
cannot be presented as sufficient to meet the queue budget. Before queue/release acceptance,
compare baseline and this head under the same Depot image, toolchain and executable cache
treatment, retaining failed samples. Do not substitute an evicted-file pass, increase a
budget, or blindly retry the failed queue job.

## Behavioral verification

The writer tests pass before and after the optimization. They cover marker rollback with
the row transaction, read/reinstall preservation, recovery of a missing individual trigger,
recovery of a missing marker, and no extra marker write when its version is already current.
The stochastic process-memory symptom is measured by the existing perf harness; a unit test
asserting an allocator/RSS number would not lock down that symptom reliably.

Final `scripts/check-fast.sh --base 63a5ed34` passed changelog, specs, hooks, clippy,
the whole Rust workspace, the fast rebuild and 1,076 Python tests (five expected failures).
Separate doctor and metadata-compatibility coverage passed 27 tests. These include the
original PR's project-requirement, read-only writer preservation and sensor-deduplication
checks; the final binary hash remained the one recorded above.
