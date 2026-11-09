---
title: "barca list"
description: "List the assets, tasks and sensors barca finds, with their inputs."
---

List every asset, task and sensor barca finds, with its kind, freshness and inputs. A scheduled
node also shows its cron and its next fire time, to the second. The time is the next match of
the cron expression in the local time of the machine `list` runs on (the table column is
`NEXT FIRE (LOCAL TIME)`). Run locally, `list` does not know a server's `--timezone`: a server started with one fires at the same wall-clock time in that zone, and its
[`GET /schedule`](/reference/server-api-endpoints/#get-schedule) reports the times it will fire at.

## Usage

```txt wrap
barca list [file.py|dir/ ...] [-l N | --all] [--json]
```

```
$ barca list pipeline.py
NAME                KIND   FRESHNESS  DEPS
------------------------------------------
pipeline.py:orders  asset  always     -
pipeline.py:total   asset  always     pipeline.py:orders
pipeline.py:notify  task   always     pipeline.py:total
```

The FRESHNESS column shows what the decorator declares. Only `schedule` has an effect, and only
under `barca serve`.

```bash
barca list                        # every node in the project
barca list pipelines/             # only files under pipelines/
barca list pipeline.py --json     # {"nodes": [{id, kind, freshness, schedule?, inputs, env, next_fire?, file, line}], "total", "truncated", "root"}
barca list pipeline.py --fields id,inputs   # JSON with only these keys per node
```

In JSON, `freshness` is `always`, `manual` or `schedule` (lowercase, like `kind`); a scheduled node
also has `schedule`, its cron expression. `list` reads no state, so it takes no `--env`.

`list` prints every node in JSON by default, in topological order; human tables show at most 100; see
[Bounded output](/reference/cli/#bounded-output).

When any node declares environment variables (`@asset(env=["SOURCE_CSV"])`), the table gains an
ENV column listing them; `--json` always includes `env` (an empty list when none are declared).


`--groups` shows the organizational hierarchy of [groups](/reference/groups/) (groups are never
executable nodes). `--server [URL]` lists a server's deployed definitions, and `--local` ignores
a configured server.

---

[All commands](/reference/cli/) · [Output format](/reference/cli/#output-format) · [Errors and exit codes](/reference/cli/#errors-and-exit-codes)
