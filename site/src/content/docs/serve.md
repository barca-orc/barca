---
title: "Running barca serve: Start Here"
description: "What barca serve does, and which page answers which question: schedules, sharing results with a team, deploying, operating, the HTTP API and alerts."
---

Assets, tasks and the cache work on one machine with no server. `barca serve` is the same
binary run as a long-lived process: it fires schedules, serves an HTTP API and a web UI, and
can hold one record of results and runs for a whole team. Start it in a project with:

```bash
barca serve                  # 127.0.0.1:8274 (--host 0.0.0.0 for other machines)
```

Authentication is off by default ([Authentication and Tokens](/auth/)).

## Which page do I need?

| I want to… | Read |
|---|---|
| Run assets, tasks or sensors on a cron schedule | [Scheduling With Cron](/scheduling/) |
| Give a team one cache and one run history | [Share Results With a Team](/team-server/) |
| Run it as a service: a container, systemd, behind nginx or Traefik | [Deploying barca serve](/deploying/) |
| Understand how laptops and the server share results, and what `--server` does | [Server Mode](/reference/server-mode/) |
| Manage the run queue, restarts, stop grace, capacity and run timeouts | [Operating barca serve](/reference/serve-ops/) |
| Start runs and read results from code, over HTTP or with the Python client | [Server API](/reference/server-api/) |
| Post to Slack when a job starts failing or recovers | [Slack Alerts](/reference/slack/) |
| Send traces and metrics to Datadog | [Telemetry](/reference/telemetry/) |
| Require tokens, and connect the web UI and laptops with them | [Authentication and Tokens](/auth/) |
| See what the web UI shows and does | [The Web UI](/web-ui/) |
| See every flag of the command | [`barca serve`](/reference/cli/serve/) |

A first setup usually goes Scheduling With Cron (if you need schedules), then Share Results With
a Team (if more than one person runs barca), then Deploying barca serve.

## Requiring a token (authentication, login)

Authentication is off by default. `barca serve --auth token` requires a token on every API
request; `barca tokens bootstrap` prints a one-use code that connects the web UI, where you
create tokens for laptops and scripts. The steps are in
[Authentication and Tokens](/auth/).
