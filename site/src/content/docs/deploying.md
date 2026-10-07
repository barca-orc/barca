---
title: Deploying barca serve
description: Run barca serve with its web UI, behind nginx, Traefik or another reverse proxy.
---

`barca serve` is the whole deployment: one process serves the HTTP API, the cron scheduler and
the web UI. There is nothing else to install — the UI is compiled into the `barca` binary.

```bash
barca serve                      # API + scheduler + web UI on 127.0.0.1:8274
open http://127.0.0.1:8274/      # redirects to /ui/
```

The UI opens on the **Assets** table: every node with its cache state (failures and stale nodes
first), last run, typical duration and next scheduled run, refreshed every 10 seconds. Click a
row for its detail panel: state, last attempt and error, run history, lineage.

## Where it listens

`barca serve` binds to `127.0.0.1:8274` by default, so only the same machine can reach it. That is
right when the proxy runs on the same host. When the proxy runs in another container, or the
server is on a VM, listen on every interface:

```bash
barca serve --host 0.0.0.0             # IPv4, every interface (`::` for IPv6)
barca serve --host 0.0.0.0 --port 8400
```

barca prints a warning when it listens on anything but loopback: there is no authentication, so
anyone who can reach the port can trigger runs. Keep the port on a private network (in Docker:
don't publish it; let only the proxy reach it) and authenticate at the proxy.

## Behind a reverse proxy

Mount barca under any path prefix. Open `https://your-host/barca/`: it redirects to
`/barca/ui/`, and the UI finds the API under the same prefix on its own — there is no base-path
setting. Strip the prefix at the proxy, so barca sees `/ui/`, `/state` and so on.

### nginx

The minimal configuration works, including live run logs:

```nginx
location /barca/ {
    proxy_pass http://127.0.0.1:8274/;       # or http://barca:8274/ on a Docker network
}
```

The trailing `/` on both sides strips the prefix.

### Traefik

A path-prefix router with a `stripPrefix` middleware. With the Docker provider, as labels on the
barca container:

```yaml
services:
  barca:
    # ... image, volumes (see below)
    command: ["barca", "serve", "--host", "0.0.0.0"]
    stop_signal: SIGINT
    labels:
      - traefik.enable=true
      - traefik.http.routers.barca.rule=PathPrefix(`/barca`)
      - traefik.http.routers.barca.middlewares=barca-strip
      - traefik.http.middlewares.barca-strip.stripprefix.prefixes=/barca
      - traefik.http.services.barca.loadbalancer.server.port=8274
```

The same routing with the file provider:

```yaml
http:
  routers:
    barca:
      rule: "PathPrefix(`/barca`)"
      middlewares: [barca-strip]
      service: barca
  middlewares:
    barca-strip:
      stripPrefix:
        prefixes: ["/barca"]
  services:
    barca:
      loadBalancer:
        servers:
          - url: "http://barca:8274"
```

Traefik needs no streaming settings: run events pass straight through.

### What the proxy has to get right

- **Live logs.** The run event stream (`GET /events/{run_id}`) sends `X-Accel-Buffering: no`,
  which tells nginx to pass it through as it happens — including with nginx's defaults and with
  `gzip` on. Without that header nginx holds the stream back and the UI shows no logs until the
  run ends, so don't add `proxy_ignore_headers X-Accel-Buffering`. Idle streams carry a
  keep-alive every 15 seconds, inside nginx's default 60-second read timeout.
- **One instance.** Runs in progress, their handles and their live events live in the server's
  memory. Point the proxy at a single `barca serve`; do not load-balance across replicas.

`tests/integration/test_reverse_proxy.sh` runs both proxies in their own containers on a Docker
network in front of `barca serve --host 0.0.0.0` (nginx with the bare `proxy_pass` above, Traefik
with the file-provider routing above) and checks, through each: the prefix redirect, the UI and
its assets, the API, and that log lines arrive while a run is still going. As a control it shows
nginx withholding the events when told to ignore the header.

## barca in a container

A minimal image: your project plus `barca`, serving on every interface.

```dockerfile
FROM python:3.12-slim
RUN pip install barca            # plus your pipeline's own dependencies
WORKDIR /app
COPY . .
EXPOSE 8274
# Until graceful shutdown lands (issue #190), SIGINT is the signal barca handles: it
# cancels in-flight runs cleanly instead of being killed mid-step after the grace period.
STOPSIGNAL SIGINT
CMD ["barca", "serve", "--host", "0.0.0.0"]
```

- **Keep `.barca/` on a volume** (`/app/.barca` here). It holds the metadata DB and, unless you
  use [remote storage](/reference/remote-storage/), the cached artifacts; on a fresh container
  every asset starts as never run.
- **Restarts drop in-flight runs.** With `STOPSIGNAL SIGINT`, `docker stop` cancels them and
  records them as `cancelled`; without it Docker's default SIGTERM is not handled yet and the
  process is killed after the grace period, which can leave those runs recorded as `running`.
  Deploy between runs where you can.

This image is a starting point, not a tested artifact: the proxy behavior above is what the
integration test covers.

## Authentication

barca has none yet. Anyone who can reach the server can trigger runs. Put authentication in front
of it at the proxy — for example nginx basic auth, Traefik's `basicAuth` or `forwardAuth`
middleware, or an SSO proxy such as oauth2-proxy:

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
barca serve --read-only --host 0.0.0.0
```

See the [Server API reference](/reference/server-api/) for every endpoint.
