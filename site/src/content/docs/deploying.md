---
title: Deploying barca serve
description: Run barca serve with its web UI, behind nginx or another reverse proxy.
---

`barca serve` is the whole deployment: one process serves the HTTP API, the cron scheduler and
the web UI. There is nothing else to install — the UI is compiled into the `barca` binary.

```bash
barca serve                      # API + scheduler + web UI on 127.0.0.1:8274
open http://127.0.0.1:8274/      # redirects to /ui/
```

The UI opens on the **Assets** table: every node with its cache state (failures and stale nodes
first), last run, typical duration and next scheduled run, refreshed every 10 seconds. Click a
row to open it in the graph.

## Behind nginx

Mount barca under any path. The minimal configuration works, including live run logs:

```nginx
location /barca/ {
    proxy_pass http://127.0.0.1:8274/;
}
```

Open `https://your-host/barca/`. It redirects to `/barca/ui/`, and the UI finds the API under
the same prefix on its own — there is no base-path setting.

- **Live logs stream through nginx's default buffering.** The run event stream
  (`GET /events/{run_id}`) sends `X-Accel-Buffering: no`, which tells nginx to pass it through
  as it happens. Without that header nginx holds the stream back and the UI shows no logs until
  the run ends — so don't add `proxy_ignore_headers X-Accel-Buffering`. Idle streams carry a
  keep-alive every 15 seconds, inside nginx's default 60-second read timeout.
- **One instance.** Runs in progress, their handles and their live events are held in the
  server's memory. Point nginx at a single `barca serve`; do not load-balance across several. A
  restart drops in-flight runs.
- **Same host.** `barca serve` listens on `127.0.0.1` only, so nginx must run on the same machine
  (or, in a container, on the host network).

`tests/integration/test_reverse_proxy.sh` checks all of this through a real nginx: the prefix,
the redirect, the UI and its assets, and that log lines arrive while a run is still going.

## Authentication

barca has none yet. Anyone who can reach the server can trigger runs. Put authentication in front
of it at the proxy — for example nginx basic auth, or an SSO proxy such as oauth2-proxy:

```nginx
location /barca/ {
    proxy_pass http://127.0.0.1:8274/;
    auth_basic "barca";
    auth_basic_user_file /etc/nginx/barca.htpasswd;
}
```

## Read-only dashboards

`barca serve --read-only` serves the UI and API without the ability to change anything: run and
cancel endpoints return `403`, the scheduler never starts, and every read of the metadata DB
goes through a private copy, so the database is never opened in place, created or written. Use it
to share a view of a project — including one another barca process is running — with people who
should only look. The UI shows a `read-only` tag and disables its run buttons.

```bash
barca serve --read-only
```

See the [Server API reference](/reference/server-api/) for every endpoint.
