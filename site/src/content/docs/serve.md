---
title: Running barca serve
description: "What barca serve does, and which page answers which question: schedules, sharing results with a team, deploying, operating, the HTTP API and alerts."
---

Assets, tasks and the cache work on one machine with no server. `barca serve` is the same
binary run as a long-lived process: it fires schedules, serves an HTTP API and a web UI, and
can hold one record of results and runs for a whole team. Start it in a project with:

```bash
barca serve                      # 127.0.0.1:8274; add --host 0.0.0.0 to reach it from elsewhere
```

It has no login. Keep it on a private network or behind a proxy that authenticates.

## Which page do I need?

| I want to… | Read |
|---|---|
| Run assets, tasks or sensors on a cron schedule | [Scheduling With Cron](/scheduling/) |
| Give a team one cache and one run history | [Share Results With a Team](/team-server/) |
| Run it as a service: a container, systemd, behind nginx or Traefik | [Deploying barca serve](/deploying/) |
| Understand how laptops and the server share results, and what `--server` does | [Server Mode](/reference/server-mode/) |
| Manage the run queue, restarts, stop grace, capacity and run timeouts | [Serve Operations](/reference/serve-ops/) |
| Start runs and read results from code, over HTTP or with the Python client | [Server API](/reference/server-api/) |
| Send traces and metrics to Datadog, or post failures to Slack | [Telemetry and Slack Alerts](/reference/telemetry/) |
| See every flag of the command | [`barca serve`](/reference/cli/serve/) |

A first setup usually goes Scheduling With Cron (if you need schedules), then Share Results With
a Team (if more than one person runs barca), then Deploying barca serve.
