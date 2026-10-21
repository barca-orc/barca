"""Interleaved median-of-N runner, for machines without hyperfine (used for the 2026-10-21 pass).

    python benchmarks/lib/interleave.py OUT.json --runs N --warmup W [--timeout S] [--prep 'cmd'] \
        --cmd 'label=shell command' [--cmd ...] [--internal label ...]

Each round runs every command once, in the order given (A B C A B C ...), so background noise
lands on all of them alike. Every command runs under `taskset -c $PIN` (default 16-31).
Wall time is measured around the subprocess; for labels passed to --internal, the
`elapsed_seconds` value the command prints as JSON is used as the measurement (wall is kept
too). A run that exits non-zero or prints "success": false is recorded as a failure.
--prep runs (unpinned, untimed) before every timed run of every command.
"""

import argparse
import json
import os
import re
import signal
import statistics
import subprocess
import sys
import time

PIN = os.environ.get("PIN", "16-31")


def loadavg():
    with open("/proc/loadavg") as f:
        return f.read().split()[:3]


def one(cmd, timeout):
    t0 = time.perf_counter()
    p = subprocess.Popen(
        ["taskset", "-c", PIN, "bash", "-c", cmd],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        start_new_session=True,
    )
    try:
        out, err = p.communicate(timeout=timeout)
    except subprocess.TimeoutExpired:
        os.killpg(p.pid, signal.SIGKILL)
        p.communicate()
        return {"wall": time.perf_counter() - t0, "ok": False, "timeout": True}
    wall = time.perf_counter() - t0
    ok = p.returncode == 0 and '"success": false' not in out
    internal = None
    m = re.findall(r'"elapsed_seconds":\s*([0-9.]+)', out)
    if m:
        internal = float(m[-1])
    r = {"wall": wall, "ok": ok, "rc": p.returncode, "internal": internal}
    r["stdout"] = out[-1500:]
    if not ok:
        r["stderr"] = err[-2000:]
    return r


def summary(xs):
    xs = sorted(xs)
    if not xs:
        return None
    q = statistics.quantiles(xs, n=4) if len(xs) >= 4 else [xs[0], statistics.median(xs), xs[-1]]
    return {
        "n": len(xs),
        "median": statistics.median(xs),
        "min": xs[0],
        "max": xs[-1],
        "q1": q[0],
        "q3": q[2],
        "mean": statistics.fmean(xs),
        "stdev": statistics.stdev(xs) if len(xs) > 1 else 0.0,
    }


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("out")
    ap.add_argument("--runs", type=int, default=10)
    ap.add_argument("--warmup", type=int, default=1)
    ap.add_argument("--timeout", type=float, default=None)
    ap.add_argument("--prep", default=None)
    ap.add_argument("--cmd", action="append", required=True)
    ap.add_argument("--internal", action="append", default=[])
    ap.add_argument("--runs-for", action="append", default=[], help="label=N overrides --runs")
    a = ap.parse_args()
    cmds = [c.split("=", 1) for c in a.cmd]
    runs_for = {k: int(v) for k, v in (x.split("=", 1) for x in a.runs_for)}
    res = {
        "pin": PIN,
        "load_before": loadavg(),
        "started": time.strftime("%Y-%m-%dT%H:%M:%S%z"),
        "env": {k: os.environ.get(k) for k in ("BARCA_POOL_SIZE", "BARCA_BENCH_WORKERS")},
        "commands": {},
    }
    for label, cmd in cmds:
        res["commands"][label] = {"cmd": cmd, "runs": [], "warmup": []}
    for _ in range(a.warmup):
        for label, cmd in cmds:
            if a.prep:
                subprocess.run(a.prep, shell=True, check=False)
            r = one(cmd, a.timeout)
            res["commands"][label]["warmup"].append(r)
            print(f"warmup {label}: {r['wall']:.3f}s ok={r['ok']}", file=sys.stderr, flush=True)
    for i in range(max([a.runs, *runs_for.values()])):
        for label, cmd in cmds:
            if i >= runs_for.get(label, a.runs):
                continue
            if a.prep:
                subprocess.run(a.prep, shell=True, check=False)
            r = one(cmd, a.timeout)
            res["commands"][label]["runs"].append(r)
            print(
                f"run {i} {label}: {r['wall']:.3f}s internal={r.get('internal')} ok={r['ok']}",
                file=sys.stderr,
                flush=True,
            )
    for label, _ in cmds:
        c = res["commands"][label]
        good = [r for r in c["runs"] if r["ok"]]
        key = "internal" if label in a.internal else "wall"
        c["metric"] = key
        c["failures"] = len(c["runs"]) - len(good)
        c["summary"] = summary([r[key] for r in good if r.get(key) is not None])
        c["wall_summary"] = summary([r["wall"] for r in good])
    res["load_after"] = loadavg()
    res["finished"] = time.strftime("%Y-%m-%dT%H:%M:%S%z")
    with open(a.out, "w") as f:
        json.dump(res, f, indent=1)
    for label, _ in cmds:
        s = res["commands"][label]["summary"]
        f = res["commands"][label]["failures"]
        if s:
            print(
                f"{label:28s} median {s['median']:.4f}s  min {s['min']:.4f}  max {s['max']:.4f}  "
                f"IQR {s['q1']:.4f}-{s['q3']:.4f}  n={s['n']} fail={f}"
            )
        else:
            print(f"{label:28s} no successful runs, fail={f}")
    print(f"load before {res['load_before']} after {res['load_after']}")


if __name__ == "__main__":
    main()
