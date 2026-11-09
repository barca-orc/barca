---
title: "barca serve"
description: "Run the HTTP API, the cron scheduler and the web UI."
---

Start an HTTP server with a JSON API, the cron scheduler and the web UI at `/ui/`. It binds to
`127.0.0.1` by default; `--host 0.0.0.0` listens on every interface. Authentication is off
unless you pass `--auth token` ([project tokens](/auth/)). See [Server API](/reference/server-api/) for the
endpoints and [Deploying](/deploying/) for running it behind nginx.

## Usage

```txt wrap
barca serve [file.py|dir/ ...] [--port N] [--host IP] [--watch] [--no-schedule] [--timezone TZ] [--read-only] [--drain-timeout SECONDS] [--max-runs N] [--capacity N] [--run-timeout SECONDS] [--auth off|token]
```

```bash
barca serve pipeline.py                 # default port 8274
barca serve pipeline.py --host 0.0.0.0 --auth token  # every interface (containers, VMs), with tokens
barca serve --timezone utc              # every file in the project; cron evaluated in UTC
barca serve pipeline.py --drain-timeout 120   # on SIGTERM, runs in flight get 2 minutes
barca serve pipeline.py --max-runs 2    # at most 2 runs at once; the others wait in the queue
barca serve pipeline.py --capacity 4    # the weight= of every run's running steps adds up to at most 4
barca serve pipeline.py --run-timeout 3600   # stop a run that takes longer than an hour
```

## Flags

| Flag | Default | Meaning |
|---|---|---|
| `-p`, `--port <PORT>` | `8274` | Port to listen on. |
| `--host <IP>` | `127.0.0.1` | IP address to bind on; `0.0.0.0` (or `::`) listens on every interface. |
| `--watch` | off | Re-parse the DAG when a source file changes. Files added later still need a restart. |
| `--no-schedule` | off | Do not fire `Schedule(...)` nodes. |
| `--timezone <TZ>` | `local` | Timezone for cron: `local`, `utc` or an IANA name such as `America/New_York`. Any other value is a usage error (exit 2). |
| `--read-only` | off | Refuse runs, never schedule; read compatible WAL metadata in place, never writing it. |
| `--drain-timeout <SECONDS>` | `[server] drain_timeout`, else `30` | On SIGTERM or SIGINT, how long runs in flight get to finish before they are put back in the run queue for the next start. `0` requeues them at once. |
| `--max-runs <N>` | `[server] max_runs`, else one per core | How many runs execute at once; the others wait in the run queue, in order (experimental). |
| `--capacity <N>` | `[server] capacity`, else none | The most the `weight=` of the running steps of every run may add up to, admitted first come, first served across runs; `GET /queue` shows it (experimental). |
| `--run-timeout <SECONDS>` | `[server] run_timeout_seconds`, else none | Stop a run that takes longer, recorded as an error naming the setting. `0` is no limit; `Schedule(..., timeout_seconds=N)` wins for its ticks. |
| `--auth <off\|token>` | `[server] auth`, else `off` | `token` requires a project bearer token on every API request ([project tokens](/auth/)); `BARCA_SERVER_AUTH` sets it too. |
| `--env <name>` | `default` | Use a named environment's cache and history. |

Every accepted run is recorded in the run queue before the server answers, and a restart resumes
the queued runs in order under their run ids. SIGTERM or SIGINT drains the server: no queued run
starts (new ones are still accepted and queued), the scheduler stops, `/health` reports
`draining`, and runs in flight finish, uploads included. The exit code is `0` when they all
finished, and `0` when `--drain-timeout` passed and the runs left were put back at the head of
the queue (they start again from the beginning after the restart). A second signal cancels them
instead (recorded as `cancelled`, reason `server shutdown`): exit code `1`. See
[Server API](/reference/server-api/#stopping-the-drain) and
[Restarting without losing work](/reference/serve-ops/#restarting-without-losing-work).

With a remote store, `barca serve` backs its metadata DB up every minute it changed (not with
`--read-only`), and `barca state restore` refuses to run while it is up. See
[Remote storage](/reference/remote-storage/).

---

[All commands](/reference/cli/) · [Output format](/reference/cli/#output-format) · [Errors and exit codes](/reference/cli/#errors-and-exit-codes)
