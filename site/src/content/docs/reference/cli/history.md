---
title: "barca history"
description: "Show recent runs, or one run with its steps and errors."
---

After verifying that the process or container has stopped, explicitly reconcile an uncertain
row with `barca history --reconcile RUN_ID --json`. This records only that row as
`interrupted` and prints it in the normal history envelope (`runs`, `total: 1`,
`truncated: false`); `--fields` and human output also work. It refuses a proven live owner,
a terminal row, an unknown id, and combining `--reconcile` with `--limit` or `--all`.
It preserves recorded step counts, successful materializations and artifacts, and leaves
unknown end time and duration as `null`. It does not stop a process. Ordinary history reads
still keep an uncertain owner `running`; reconciliation is your assertion that it stopped.
The change is local to the selected environment's metadata database.

## Usage

```txt wrap
barca history [RUN_ID] [--status S] [--target NAME] [--triggered-by T] [--since WHEN] [-l N | --all] [--json|--pretty]
```


Show recent runs from `.barca/metadata.db`: run id, command, target, status, step counts and
timing. TARGET is the run's job by function name, `all` for a run without targets, cut at 24
characters with `…`.
The human table defaults to the 10 most recent; JSON returns every run unless `--limit` is explicit.

```
$ barca history
RUN_ID         CMD     TARGET                   STATUS      STEPS CACHED   TIME STARTED
------------------------------------------------------------------------------------------------------
14f1e316840c   run     notify                   success         1      2   0.1s 2026-10-21 02:39:25 UTC
14f01f296a75   get     country_report,weekly_s… success         0      2   0.0s 2026-10-21 02:39:25 UTC
14f3870e173f   get     all                      success         2      0   0.1s 2026-10-21 02:39:25 UTC
```

Times are UTC. `--fields` takes `run_id`, `command`, `files`, `target`, `job`, `status`,
`steps_total`, `steps_executed`, `steps_cached`, `attempts`, `started_at`, `finished_at`,
`elapsed_seconds`, `triggered_by`, `scheduled_at`, `schedule_id`, `client_run_id`, `failure`. `triggered_by` says what
started the run: `manual` (a command typed on a machine), `api` (an HTTP request to
`barca serve`), `schedule` (a cron tick, with the tick in `scheduled_at`, RFC 3339 in UTC, and
the scheduled node in `schedule_id`) or `client`; it is `null` for runs recorded before barca
recorded it. `job` is the run's job in full: `all`, or its resolved node ids sorted and
comma-separated (`null` for runs recorded before 0.29.0); `target` is the target as typed.

```bash
barca history --fields run_id,triggered_by,scheduled_at   # what started each run
barca history -l 25               # last 25; human default 10, JSON default every run
barca history --json              # {"runs": [...], "total": N, "truncated": bool, "hint"?: "..."}
barca history --fields run_id,status   # JSON with only these keys per run
```

History JSON returns every run unless `--limit N` is explicit; the human table defaults to 10.
When more runs exist than are shown, the JSON has `"truncated": true`, the `total`, and a `hint`;
the table prints the same hint as one line on stderr. See [Bounded output](/reference/cli/#bounded-output). Each
run's `files` is an array of the `.py` files it was given.

Terminal status and successful/failed step rows commit together. Required database-write
failures report an infrastructure error and preserve previous progress/history and
run state, which normally remains unfinished. Captured output is written separately after that transaction.

Mid-run database writes commit completed-step rows and their progress count together.
Failed batches are retained and retried without waiting for another step; replay does
not count a completed step twice. Progress can be absent while the database cannot commit.
The final write still records completed results not saved during execution.

## Finding and reading a failed run

Filters combine with AND, and `--limit`, `total` and `truncated` count the runs they keep:
`--status` (comma-separated: `success`, `failed`, `cancelled`, `error`, `running`,
`interrupted`), `--target NAME` (the run's target or job is that node; runs of every node are
left out), `--triggered-by` (comma-separated: `manual`, `api`, `schedule`, `client`) and
`--since` (`30m`, `24h`, `7d`, `2w`, an RFC 3339 time or a UTC date). A failed run's row ends
with its first failed step and the first line of its error; JSON runs carry it as
`failure: {node, error}` and as the flat `failed_node` and `error` (`null` otherwise), so
`--fields run_id,failed_node,error` needs no nesting.

```bash
barca history --status failed --since 24h
barca history --target clean --triggered-by schedule --json
barca history 836fdca186c8          # one run in detail
barca history 836fdca186c8 --json   # {run, steps: [...], hint?}
```

`barca history RUN_ID` prints the run's fields, the steps it recorded (status, attempts, time,
peak memory, run hash, artifact path, format and size), and for each failed step its error,
traceback, when the worker died how it ended (`worker exit: SIGKILL (possibly the OOM
killer)`), and the last 20 lines it printed (`output_tail` in JSON, without the traceback).
Each step's JSON has `max_rss_bytes`, the worker process's peak resident memory while the step
ran (on Linux the step's own; elsewhere only when the step raised the worker's peak), and
`cpu_seconds`. Bytes DuckDB spilled to disk are not recorded: DuckDB keeps no total of them. A hint on
stderr names `barca logs RUN_ID --step STEP --stream stderr`. Cached steps are counted in
`steps_cached`, not listed: a run records no row for them. With a server configured, both
forms answer from the server; a run only this machine has is an error whose remediation is
`barca history RUN_ID --local`.

```
$ barca history 836fdca186c8
run 836fdca186c8  get clean  failed  0.1s  1 attempt
started 2026-10-22 22:57:27.000 UTC  finished 2026-10-22 22:57:27.000 UTC
triggered by api
files pipeline.py
steps: 1 executed (1 failed), 1 cached (cached steps are not listed)

STEP               STATUS  ATTEMPTS  TIME  RUN_HASH      ARTIFACT
pipeline.py:clean  failed  1         -     d64e1b00647e  -

pipeline.py:clean failed: ZeroDivisionError: division by zero
  Traceback (most recent call last):
    File "/srv/pipelines/pipeline.py", line 11, in clean
      return [{"id": o["id"], "per_unit": 10 / o["qty"]} for o in orders]
                                          ~~~^~~~~~~~~~
  ZeroDivisionError: division by zero
hint: barca logs 836fdca186c8 --step clean --stream stderr
```

A run's `status` is `running`, `success`, `failed`, `cancelled` or `interrupted`. A run records
each step as it finishes, so a `running` run already counts them in `steps_executed`, and
`barca status` from another terminal shows them as `cached`. A run whose process was killed is
`interrupted` (no `finished_at`); the next `barca get` reuses the steps it had recorded. A run
killed in a container is `interrupted` too, once a container starts again on the same `.barca`
volume (from 0.20.0; earlier versions left it `running`). For runs started with 0.20.0 or later, a run stays `running`
whenever barca cannot establish that its process is gone. Earlier runs retain their legacy
process-id and host-name checks. See
`barca docs layout`, "While a run is going, and after one is killed", and the shared-state
[limitations](/reference/remote-storage/#limitations).

---

[All commands](/reference/cli/) · [Output format](/reference/cli/#output-format) · [Errors and exit codes](/reference/cli/#errors-and-exit-codes)
