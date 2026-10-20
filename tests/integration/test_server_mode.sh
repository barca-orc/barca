#!/usr/bin/env bash
# `--server` through a reverse proxy mounted under a path prefix (#394): the client keeps the
# prefix on every route, streams the run's events through the proxy, and prints the same as
# when it talks to barca directly. nginx runs in a container: on Linux on the host's network
# (so a host firewall between containers and the host does not matter), elsewhere on a bridged
# network reaching barca through host.docker.internal, with barca on `--host 0.0.0.0`.
#
# Needs docker; skips (exit 0) without it.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
BARCA="${REPO_ROOT}/.venv/bin/barca"
[ -x "$BARCA" ] || BARCA="$(command -v barca)"

if ! command -v docker >/dev/null 2>&1 || ! docker info >/dev/null 2>&1; then
    echo "SKIP: docker is not available"
    exit 0
fi

TMP="$(mktemp -d)"
BARCA_PORT=8294
NGINX_PORT=8295
NGINX="barca-server-mode-nginx-$$"
cleanup() {
    docker rm -f "$NGINX" >/dev/null 2>&1 || true
    [ -n "${SERVE_PID:-}" ] && kill "$SERVE_PID" 2>/dev/null || true
    rm -rf "$TMP"
}
trap cleanup EXIT

for port in "$BARCA_PORT" "$NGINX_PORT"; do
    if (exec 3<>"/dev/tcp/127.0.0.1/$port") 2>/dev/null; then
        echo "FAIL: port $port is already in use"
        exit 1
    fi
done

mkdir -p "$TMP/served" "$TMP/laptop"
cat > "$TMP/served/pipeline.py" <<'PY'
import time

from barca import asset, task


@asset()
def numbers() -> list:
    return [1, 2, 3]


@asset(inputs={"rows": numbers})
def total(rows: list) -> dict:
    for i in range(3):
        print(f"tick {i}", flush=True)
        time.sleep(0.5)
    return {"total": sum(rows)}


@task(inputs={"t": total})
def broken(t: dict) -> None:
    raise ValueError("through the proxy")
PY
cp "$TMP/served/pipeline.py" "$TMP/laptop/pipeline.py"

if [ "$(uname -s)" = Linux ]; then
    UPSTREAM="127.0.0.1:${BARCA_PORT}"
    NET=(--network host)
    HOST=127.0.0.1
else
    UPSTREAM="host.docker.internal:${BARCA_PORT}"
    NET=(--add-host host.docker.internal:host-gateway -p "${NGINX_PORT}:${NGINX_PORT}")
    HOST=0.0.0.0
fi

(cd "$TMP/served" && exec "$BARCA" serve pipeline.py --host "$HOST" --port "$BARCA_PORT" --no-schedule >"$TMP/serve.log" 2>&1) &
SERVE_PID=$!

cat > "$TMP/nginx.conf" <<NGINX
events {}
http {
    server {
        listen ${NGINX_PORT};
        location /barca/ {
            proxy_pass http://${UPSTREAM}/;
        }
    }
}
NGINX
docker run -d --name "$NGINX" "${NET[@]}" \
    -v "$TMP/nginx.conf:/etc/nginx/nginx.conf:ro,Z" nginx:1.27-alpine >/dev/null

for _ in $(seq 1 60); do
    curl -sf "http://127.0.0.1:${NGINX_PORT}/barca/health" >/dev/null 2>&1 && break
    sleep 0.5
done

DIRECT="http://127.0.0.1:${BARCA_PORT}"
PROXIED="http://127.0.0.1:${NGINX_PORT}/barca/"
fail() { echo "FAIL: $*"; cat "$TMP/serve.log"; exit 1; }
cd "$TMP/laptop"

# A run through the proxy: its step's lines arrive, its value is printed, exit 0.
"$BARCA" get total pipeline.py --pretty --server "$PROXIED" >"$TMP/out" 2>"$TMP/err" \
    || fail "get through the proxy exited $?: $(cat "$TMP/err")"
grep -q '^\[pipeline.py:total\] tick 2$' "$TMP/err" || fail "the step's lines did not stream: $(cat "$TMP/err")"
grep -q '"total": 6' "$TMP/out" || fail "no value: $(cat "$TMP/out")"

# A failed step through the proxy: exit 1 with the envelope, as directly.
set +e
"$BARCA" run broken pipeline.py --json --server "$PROXIED" >"$TMP/out" 2>"$TMP/err"
code=$?
set -e
[ "$code" = 1 ] || fail "run broken exited $code, not 1: $(cat "$TMP/err")"
grep -q '"kind":"step_failed"' "$TMP/err" || fail "no step_failed envelope: $(cat "$TMP/err")"

# Inspection prints the same through the proxy as directly.
for cmd in "list pipeline.py --json" "history --json" "status pipeline.py --json" "stats total pipeline.py --json"; do
    # shellcheck disable=SC2086
    direct="$("$BARCA" $cmd --server "$DIRECT")"
    # shellcheck disable=SC2086
    proxied="$("$BARCA" $cmd --server "$PROXIED")"
    [ "$direct" = "$proxied" ] || fail "barca $cmd differs through the proxy"
done

# A wrong prefix is not barca: exit 3 naming the URL.
set +e
"$BARCA" history --json --server "http://127.0.0.1:${NGINX_PORT}/elsewhere" >/dev/null 2>"$TMP/err"
code=$?
set -e
[ "$code" = 3 ] || fail "a wrong prefix exited $code, not 3"
grep -q "/elsewhere" "$TMP/err" || fail "the error does not name the URL: $(cat "$TMP/err")"

echo "PASS: --server through a path-prefix proxy"
