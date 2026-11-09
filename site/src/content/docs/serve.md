---
title: Running barca serve
description: "What barca serve does, and which page answers which question: schedules, sharing results with a team, deploying, operating, the HTTP API and alerts."
---

Assets, tasks and the cache work on one machine with no server. `barca serve` is the same
binary run as a long-lived process: it fires schedules, serves an HTTP API and a web UI, and
can hold one record of results and runs for a whole team. Start it in a project with:

```bash
barca serve                  # 127.0.0.1:8274 (--host 0.0.0.0 for other machines)
```

Authentication is off by default ("Requiring a token" below).

## Which page do I need?

| I want to… | Read |
|---|---|
| Run assets, tasks or sensors on a cron schedule | [Scheduling With Cron](/scheduling/) |
| Give a team one cache and one run history | [Share Results With a Team](/team-server/) |
| Run it as a service: a container, systemd, behind nginx or Traefik | [Deploying barca serve](/deploying/) |
| Understand how laptops and the server share results, and what `--server` does | [Server Mode](/reference/server-mode/) |
| Manage the run queue, restarts, stop grace, capacity and run timeouts | [Serve Operations](/reference/serve-ops/) |
| Start runs and read results from code, over HTTP or with the Python client | [Server API](/reference/server-api/) |
| Post to Slack when a job starts failing or recovers | [Slack Alerts](/reference/slack/) |
| Send traces and metrics to Datadog | [Telemetry](/reference/telemetry/) |
| Require tokens, and connect the web UI and laptops with them | [Server Mode: project tokens](/reference/server-mode/#opt-in-project-tokens) |
| See what the web UI shows and does | [The Web UI](/web-ui/) |
| See every flag of the command | [`barca serve`](/reference/cli/serve/) |

A first setup usually goes Scheduling With Cron (if you need schedules), then Share Results With
a Team (if more than one person runs barca), then Deploying barca serve.

## Requiring a token (authentication, login)

With authentication off, anyone who can reach the port can start and cancel runs. To require a
credential on every API request, health checks included:

```bash
barca serve --auth token          # or [server] auth = "token" in barca.toml, or BARCA_SERVER_AUTH=token
barca tokens bootstrap            # on the server's host: prints a one-use code, valid 15 minutes
```

Open the web UI and paste the code on its Connect screen, then create named API tokens under
**API tokens** (each secret is shown once, and can be revoked there). A laptop or script sends one
with `BARCA_SERVER_TOKEN=<token>`. The details, including the HTTP routes for scripts, are in
[Server Mode: project tokens](/reference/server-mode/#opt-in-project-tokens). Without tokens,
keep the server on a private network or behind a proxy that authenticates
([Deploying](/deploying/#authentication)).
