---
title: "barca status"
description: "Cache state and why, last run and artifact shape for each node."
---

Show, for every node in scope, its kind, inputs, whether it is partitioned, its cache state
with the reason, its last run, and the shape of its result. It runs nothing.

```bash
barca status total pipeline.py                   # only `total` and its upstream cone
barca status total,notify pipeline.py            # several targets: the union of their cones
barca status pipeline.py --json                  # {target, targets, nodes, summary, total, truncated, root}
barca status pipeline.py --json --sample 5       # plus up to 5 sample rows per json/parquet/CSV artifact
barca status --problems                          # only nodes that need attention
barca status --state stale,never_run             # only these cache states
```

`--problems` keeps every node that is not fine: every state but `cached` and `always_runs`, a
task or sensor whose last run failed, and a schedule that is paused or failing (`max_failures`
reached). `--state` keeps the named states. The summary still counts every node, and the JSON
names the filter in `filter`. A node whose last run failed shows the first line of its error in
WHY (`failed: ZeroDivisionError: division by zero`); the whole message is
`last_materialization.error`. A scheduled node has `schedule` (`cron`, `next_fire`,
`failures`, `max_failures`, `failing`, `paused`, `paused_until`, `last_status`, `live`) and the
table a SCHEDULE column: from a server it is the server's scheduler's view, the numbers of
`GET /schedule` and the web UI; on this machine it is what the metadata DB holds, with
`live: false`. See [Agent Skill](/reference/agent-skill/) and `barca docs agents`,
"Debugging pipelines".

```
NAME    KIND   STATE        WHY           LAST RUN                               SHAPE            DEPS
orders  asset  cached       materialized  success 2026-10-21 02:39:25 UTC 0.00s  3 rows x 2 cols  -
total   asset  cached       materialized  success 2026-10-21 02:39:25 UTC 0.00s  dict (1 key)     orders
notify  task   always-runs  task          success 2026-10-21 02:39:25 UTC 0.00s  dict (1 key)     total

2 cached, 0 stale, 0 never run, 0 partial, 0 unknown, 1 always run
```

**Cache state** (`cache.state`) is the decision `--dry-run` makes, from the same code path:
`cached`, `unsynced`, `stale`, `never_run`, `partial`, `blocked`, `unknown` or `always_runs`,
each with a `reason` (`changed`, `upstream_stale`, `local_only`, `partitions_unknown`, ...) and
a `detail`. The table of states, reasons and what `cache.changed` lists is owned by
[Status](/reference/status/#cache-state) (`barca docs status`).

**Last materialization** is the most recent execution recorded in the metadata DB, success or
failure. Cache hits do not change it. For a partitioned node it is the latest run's first key in
sort order, so identical runs report the same key.

**Shape** is read from the artifact file only, by a small Python helper; your code is never
imported. Parquet gives `rows` and `columns` from the file footer (needs pyarrow), json gives
`type` and, for a list or object, `rows`, `columns` or `keys`, and pickle gives `type` only.
Remote artifacts are read from the store; a failure is reported in `shape.note` and the
command still exits 0. Details: `barca docs status`.

**Partitioned assets** appear as one node with `partitions: {total, cached, missing,
missing_keys}` (up to 20 keys). `last_materialization` is the most recently run key (named in its
`partition` field), and `shape` describes that key's artifact.

Each node also lists `env`, the environment variables it declares with `env=[...]`. Like `list`,
status returns every node in JSON unless `--limit N` is explicit. Its human table shows at most
100 nodes unless you pass `--limit N` or `--all`; the `summary` still counts
every node, and the JSON reports `total` and `truncated` (see [Bounded output](/reference/cli/#bounded-output)).

With an [artifact store](/reference/remote-storage/), a cached unpartitioned node is `cached`
only when this machine's copy, the store's object and the recorded digest agree; otherwise it is
`unsynced` with `cache.reason` `local_only`, `store_only` or `mismatch`, and `barca get` repairs
it without running a step.
Status reads the metadata DB as it is at that moment: while a `barca get` is running, the
steps it has finished already show as `cached` (a partitioned asset as `partial`).

Status executes no pipeline step and records no new run; `partitions(<expression>)` may evaluate Python while
resolving keys. An unknown target
is a usage error (exit 2). See `barca docs status`.

---

[All commands](/reference/cli/) · [Output format](/reference/cli/#output-format) · [Errors and exit codes](/reference/cli/#errors-and-exit-codes)
