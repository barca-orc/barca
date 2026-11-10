---
title: "barca stats"
description: "Timing and cache statistics for one asset."
---

Show statistics for one node: how many times it executed, timings (average, median, p95,
max), how many executions repeated unchanged inputs, and recent runs. The target is required.
Times are UTC.

## Usage

```txt wrap
barca stats <target> [file.py|dir/ ...]
```

```
$ barca stats total pipeline.py
Asset: pipeline.py:total
Total materializations: 1
Timing:  avg 0.000s  median 0.000s  p95 0.000s  max 0.000s
Re-executed with unchanged inputs: 0.0%
Peak memory (RSS):  median 27.8 MB  max 27.8 MB

Recent runs:
  ELAPSED    STATUS    ATTEMPTS PEAK_RSS   RUN          CREATED
  0.000s     success   1        27.8 MB    792c575938f8 2026-11-09 20:05:36 UTC
```

A cache hit executes nothing, so it is not counted here; `barca history` counts cache hits per
run. In the JSON the re-execution share is `cache_hit_rate` (the key keeps its name).

The JSON has `id`, `total_runs`, `cache_hit_rate`, `avg_elapsed_seconds`,
`median_elapsed_seconds`, `p95_elapsed_seconds`, `max_elapsed_seconds`, `max_rss_bytes` and
`median_max_rss_bytes` (peak memory over the recorded executions) and `recent_runs` (each
`elapsed_seconds`, `status`, `created_at`, `error_message`, `attempts`, `run_id`, `cpu_seconds`,
`max_rss_bytes`). `run_id` names the run, whose every step `barca history RUN_ID` lists.

```bash
barca stats summary pipeline.py --json     # one JSON object; the node id is `id`
barca stats summary pipeline.py --fields status,error_message   # JSON; trims recent_runs entries
barca stats summary --fields run_id,elapsed_seconds,max_rss_bytes   # each recent run's id, time and peak memory
```

---

[All commands](/reference/cli/) · [Output format](/reference/cli/#output-format) · [Errors and exit codes](/reference/cli/exit-codes/)
