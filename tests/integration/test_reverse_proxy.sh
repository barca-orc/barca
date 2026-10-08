#!/usr/bin/env bash
# barca serve behind a reverse proxy mounted under a path prefix — the
# deployments the docs describe (site/src/content/docs/deploying.md). nginx and
# Traefik each run in their own container on a bridged Docker network and reach
# barca over that network, so barca listens with `--host 0.0.0.0` (on Linux a
# loopback-only barca is unreachable from a bridged container).
#
# Checks, through each proxy:
#   - `/barca/` redirects to `/barca/ui/`, and the UI and its assets load there;
#   - the API works under the prefix;
#   - run events stream live: log lines arrive while the run is still going.
# And a control: when nginx is told to ignore barca's `X-Accel-Buffering: no`,
# it withholds the events — so the streaming check really measures the header.
#
# Needs docker; skips (exit 0) without it. The UI checks need a binary built
# with the web UI (`pnpm --dir ui build` before maturin); without one they are
# skipped and the API/streaming checks still run.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
BARCA="${REPO_ROOT}/.venv/bin/barca"
[ -x "$BARCA" ] || BARCA="$(command -v barca)"
PY="${REPO_ROOT}/.venv/bin/python"
[ -x "$PY" ] || PY="$(command -v python3)"

if ! command -v docker >/dev/null 2>&1 || ! docker info >/dev/null 2>&1; then
    echo "SKIP: docker is not available"
    exit 0
fi

TMP="$(mktemp -d)"
BARCA_PORT=8291
NGINX_PORT=8292
TRAEFIK_PORT=8293
NGINX="barca-proxy-nginx-$$"
TRAEFIK="barca-proxy-traefik-$$"
cleanup() {
    docker rm -f "$NGINX" "$TRAEFIK" >/dev/null 2>&1 || true
    [ -n "${SERVE_PID:-}" ] && kill "$SERVE_PID" 2>/dev/null || true
    rm -rf "$TMP"
}
trap cleanup EXIT

for port in "$BARCA_PORT" "$NGINX_PORT" "$TRAEFIK_PORT"; do
    if (exec 3<>"/dev/tcp/127.0.0.1/$port") 2>/dev/null; then
        echo "FAIL: port $port is already in use — another server would answer these checks"
        exit 1
    fi
done

cat > "$TMP/pipeline.py" <<'PY'
import time

from barca import asset


def _tick() -> int:
    for i in range(4):
        print(f"tick {i}", flush=True)
        time.sleep(1)
    return 4


@asset()
def slow() -> int:
    return _tick()


@asset()
def slow_b() -> int:
    return _tick()


@asset()
def slow_c() -> int:
    return _tick()
PY

# `exec` so $! is barca itself, and cleanup stops it rather than just the subshell.
(cd "$TMP" && exec "$BARCA" serve pipeline.py --host 0.0.0.0 --port "$BARCA_PORT" >"$TMP/serve.log" 2>&1) &
SERVE_PID=$!

# The proxies reach the host by name over the bridge network. Docker Desktop
# provides host.docker.internal; on Linux, host-gateway maps it to the bridge
# gateway — which only reaches barca because it listens on 0.0.0.0.
UPSTREAM="host.docker.internal:${BARCA_PORT}"
HOSTS=(--add-host host.docker.internal:host-gateway)

cat > "$TMP/nginx.conf" <<NGINX
events {}
http {
    server {
        listen ${NGINX_PORT};
        # The least a user writes: nginx defaults (HTTP/1.0 upstream, buffering
        # on). Live events then depend on barca's X-Accel-Buffering header.
        location /barca/ {
            proxy_pass http://${UPSTREAM}/;
        }
        # Control: the same, but nginx ignores that header.
        location /buffered/ {
            proxy_pass http://${UPSTREAM}/;
            proxy_ignore_headers X-Accel-Buffering;
        }
    }
}
NGINX

# Traefik: a path-prefix router with the prefix stripped before it reaches barca.
cat > "$TMP/traefik.yml" <<TRAEFIK
http:
  routers:
    barca:
      rule: "PathPrefix(\`/barca\`)"
      entryPoints: [web]
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
          - url: "http://${UPSTREAM}"
TRAEFIK

# Label the temporary config mounts for SELinux hosts (also supported by Docker).
docker run -d --name "$NGINX" "${HOSTS[@]}" -p "${NGINX_PORT}:${NGINX_PORT}" \
    -v "$TMP/nginx.conf:/etc/nginx/nginx.conf:ro,Z" nginx:1.27-alpine >/dev/null
docker run -d --name "$TRAEFIK" "${HOSTS[@]}" -p "${TRAEFIK_PORT}:${TRAEFIK_PORT}" \
    -v "$TMP/traefik.yml:/etc/traefik/dynamic.yml:ro,Z" traefik:v3.1 \
    "--entryPoints.web.address=:${TRAEFIK_PORT}" \
    --providers.file.filename=/etc/traefik/dynamic.yml >/dev/null

for port in "$NGINX_PORT" "$TRAEFIK_PORT"; do
    for _ in $(seq 1 60); do
        curl -sf "http://127.0.0.1:${port}/barca/health" >/dev/null 2>&1 && break
        sleep 0.5
    done
done

"$PY" - "$NGINX_PORT" "$TRAEFIK_PORT" <<'PY'
import json
import re
import sys
import time
import urllib.error
import urllib.request

nginx_port, traefik_port = sys.argv[1], sys.argv[2]
failures = 0


def check(ok, msg):
    global failures
    print(("  ok  " if ok else "  FAIL ") + msg)
    failures += not ok


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, *a, **k):
        return None


opener = urllib.request.build_opener(NoRedirect)


def event_arrivals(base, prefix, target, read_timeout=30):
    """Start a run and return the arrival times (s) of its log events. The event
    stream stays open after the run (keep-alives), so stop at run_finished — or
    at a read timeout, when the proxy is holding the stream back."""
    req = urllib.request.Request(f"{base}/{prefix}/get/{target}", method="POST")
    handle = json.load(urllib.request.urlopen(req))["run_id"]
    t0 = time.monotonic()
    times = []
    try:
        with urllib.request.urlopen(
            f"{base}/{prefix}/events/{handle}", timeout=read_timeout
        ) as stream:
            for raw in stream:
                line = raw.decode().strip()
                if not line.startswith("data:"):
                    continue
                ev = json.loads(line[5:])
                if ev["type"] == "log":
                    times.append(time.monotonic() - t0)
                if ev["type"] == "run_finished":
                    break
    except TimeoutError:
        pass
    return times


def proxy_checks(name, base, target):
    print(f"{name}:")
    try:
        opener.open(f"{base}/barca/")
        check(False, "/barca/ redirects")
    except urllib.error.HTTPError as e:
        loc = e.headers.get("Location", "")
        check(
            e.code in (301, 302, 303, 307, 308) and loc.endswith("ui/"),
            f"/barca/ redirects to ui/ (Location: {loc})",
        )

    health = json.load(urllib.request.urlopen(f"{base}/barca/health"))
    check(health.get("status") == "ok", "API under the prefix: /barca/health")
    state = json.load(urllib.request.urlopen(f"{base}/barca/state"))
    check(
        isinstance(state, list) and any(n["name"] == "slow" for n in state),
        "API under the prefix: /barca/state",
    )

    try:
        page = urllib.request.urlopen(f"{base}/barca/ui/").read().decode()
        assets = re.findall(r'(?:src|href)="\./([^"]+)"', page)
        check(bool(assets), f"UI page at /barca/ui/ references relative assets ({len(assets)})")
        for a in assets:
            r = urllib.request.urlopen(f"{base}/barca/ui/{a}")
            check(r.status == 200, f"asset /barca/ui/{a} loads ({r.headers.get('Content-Type')})")
    except urllib.error.HTTPError as e:
        if e.code == 404 and b"built without its web UI" in e.read():
            print("  skip UI checks: this binary was built without the web UI")
        else:
            raise

    live = event_arrivals(base, "barca", target)
    check(len(live) == 4, f"4 log events streamed ({len(live)})")
    spread = live[-1] - live[0] if live else 0
    check(spread > 2.0, f"events arrive as they happen (first→last {spread:.1f}s)")


nginx = f"http://127.0.0.1:{nginx_port}"
proxy_checks("nginx (bare proxy_pass)", nginx, "slow")
proxy_checks("Traefik (PathPrefix + stripPrefix)", f"http://127.0.0.1:{traefik_port}", "slow_b")

# Control: with the header ignored, nginx holds the stream back, so no event
# arrives while the run is going (the run takes ~4s) — the nginx streaming check
# above really measures the header.
print("control:")
held = event_arrivals(nginx, "buffered", "slow_c", read_timeout=8)
during = [t for t in held if t < 4.5]
check(not during, f"nginx withholds events when the header is ignored ({len(during)} arrived during the run)")

sys.exit(1 if failures else 0)
PY
