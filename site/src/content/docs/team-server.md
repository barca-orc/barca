---
title: Share Results With a Team
description: One barca serve and one bucket give a whole team the same cache and run history, in three steps.
---

Once one person has computed something, nobody else has to compute it again. You run one
`barca serve` that everyone points at, and you keep using `barca get` exactly as before.

## Only need to run on the server?

Start `barca serve --host 0.0.0.0` in the deployed project, then use these commands from
your laptop (replace the address with your server):

```bash
barca get revenue --server http://barca.internal:8274
barca run publish --server http://barca.internal:8274
barca history --server http://barca.internal:8274
```

This executes the server's copy of the project and keeps results and history there. It
needs no bucket on your laptop and does not upload local edits. Keep the server on a
private network or behind an authenticating proxy; it has no login.

## Also want laptops to compute against the same cache?

Follow the three steps below. With this setup, plain `barca get` computes on the laptop
and records the run on the server; `barca get --server` computes on the server. Both use
one shared record of results and runs. `--local` deliberately uses a separate local record.

You need:

- a machine your team can reach, such as a small VM or a box on your VPN or Tailscale
- a bucket on S3, GCS or Azure that the server and every laptop can read and write
- the project in git, so the server and the laptops have the same code

## 1. Add two sections to `barca.toml`

```toml
# barca.toml, committed with the project
[server]
url = "http://barca.internal:8274"   # where step 2 runs

[remote]
uri = "s3://team-bucket/barca"       # gs:// and az:// work too
```

Commit it. Everyone, the server included, uses this same file. `barca serve` ignores the
`[server]` section, and each laptop reads it to find the server.

Install the extra for your bucket, for example with `uv add 'barca[s3]'` (or `barca[gcs]`
or `barca[azure]`). Bucket credentials come from the usual places, such as `AWS_PROFILE`, so
`barca docs remote` has the details.

## 2. Start the server

On the server, in a checkout of the project:

```bash
uv sync
uv run barca serve --host 0.0.0.0
```

```
...
[barca] serving on http://0.0.0.0:8274  (1 file)
[barca] warning: listening on 0.0.0.0 with no authentication — anyone who can reach
port 8274 can trigger runs
```

The web UI is at `http://barca.internal:8274/`. There is no login, so keep the server on a
private network or behind a proxy that authenticates ([Deploying](/deploying/)).

## 3. Use barca as usual

On your laptop nothing changes. The first `barca get` computes what is new and records it on
the server:

```
$ barca get revenue
[barca] server http://barca.internal:8274 ([server] url in barca.toml)
[barca] 2/2 steps | done in 2.1s
...
Run 90af4d65f284 | got 'revenue' in 2.068s (2 steps, 1 phase)

.barca/artifacts/pipeline.py--revenue/43fbf60a1f6e...json
  {
    "total": 42
  }
```

Your teammate runs the same command in their own checkout. Nothing runs: both results come
from the server's record, and the one they asked for is downloaded from the bucket:

```
$ barca get revenue
[barca] server http://barca.internal:8274 ([server] url in barca.toml)
[barca] checking artifact store s3://team-bucket/barca/default/artifacts (timeout 10s)
[barca] fetched 1 cached artifact (13 B) in 0.0s
Run 9025447e887c | got 'revenue' in 0.048s (0 steps, 1 phase)

.barca/artifacts/pipeline.py--revenue/43fbf60a1f6e...json
  {
    "total": 42
  }
```

If they have edited `revenue`, only `revenue` runs on their machine. `orders` still comes from
the cache, because a result is shared whenever the code and inputs match.

## What else you can do

```bash
barca status                 # what the team has, and each result's shape
barca history                # everyone's runs
barca history --status failed --since 24h   # what failed today, each with its step and error
barca history 836fdca186c8   # one run: its steps, the error and traceback, and the logs command
barca sql "select * from revenue"   # query the team's results here (the server runs no SQL)
barca get revenue --server   # run it on the server
barca get revenue --local    # skip the server once
```

```
$ barca history -l 3
RUN_ID         CMD     TARGET                   STATUS      STEPS CACHED   TIME STARTED
------------------------------------------------------------------------------------------------------
9086ad528e03   get     revenue                  success         0      2   0.0s 2026-10-22 19:00:53 UTC
9085f266d644   get     revenue                  success         2      0   2.1s 2026-10-22 19:00:46 UTC
```

```
$ barca history --status failed
RUN_ID         CMD     TARGET                   STATUS      STEPS CACHED   TIME STARTED
------------------------------------------------------------------------------------------------------
836f80e59467   get     enrich                   failed          1      1   0.1s 2026-10-22 22:57:27 UTC  enrich: ConnectionError: pricing API timed out
836fdca186c8   get     clean                    failed          1      1   0.1s 2026-10-22 22:57:27 UTC  clean: ZeroDivisionError: division by zero
```

`--server` runs the server's copy of the code, not your local edits, and prints the output here.
Use it for heavy jobs, or for anything that must run where the data lives. From Python,
`barca.get("revenue")` follows the same `barca.toml`, and `barca.get("revenue", server=True)`
runs it on the server.

Schedules run on the server. Give an asset `freshness=Schedule("0 6 * * *")` and the server
brings it up to date every day at 06:00 ([Scheduling](/scheduling/)).

## If something goes wrong

| You see | It means | Do |
|---|---|---|
| `the barca server at ... did not answer GET /health` (exit 3) | the server is down or unreachable | start it, or add `--local` to work alone for now |
| `a server is configured (...) but this project has no artifact store` (exit 2) | the laptop has `[server]` but no `[remote]` | add the `[remote]` section from step 1 |
| `the barca server at ... writes artifacts to ..., this project to ...` (exit 2) | the server and the laptop use different buckets | use the one committed `barca.toml` |
| `warning: run ... was not recorded on the server` | your run worked, but the server did not get it | nothing: the next command sends it again |

Every message ends with the fix, and `--local` is always offered. barca never quietly falls back to your own cache when the server is down, because two people
who stop agreeing without noticing is what the server is there to prevent. `--local` says
you mean it.

## Next

- [Deploying](/deploying/): run the server under systemd or Docker, behind nginx or Traefik.
- [Server Mode reference](/reference/server-mode/): every rule, including what is shared,
  what happens when a post fails, and the HTTP routes. In the terminal, run
  `barca docs server-mode`.
