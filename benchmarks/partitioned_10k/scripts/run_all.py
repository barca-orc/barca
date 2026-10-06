"""Run all framework benchmarks sequentially: 10 steps × 1000 partitions.

Each framework uses its idiomatic partition/map pattern.
Timeout: 2 minutes per framework.
"""

import json
import os
import shutil
import signal
import subprocess
import time

TICKERS = [f"T{i:04d}" for i in range(1000)]
TIMEOUT = 120  # 2 minutes


class TimeoutError(Exception):
    pass


def timeout_handler(signum, frame):
    raise TimeoutError()


def timed_run(name, fn):
    """Run a benchmark function with a timeout."""
    print(f"\n{'=' * 60}")
    print(f"  {name}")
    print(f"{'=' * 60}")
    signal.signal(signal.SIGALRM, timeout_handler)
    signal.alarm(TIMEOUT)
    try:
        t0 = time.perf_counter()
        result = fn()
        elapsed = time.perf_counter() - t0
        signal.alarm(0)
        print(f"  Result: {elapsed:.3f}s")
        return {"framework": name, "elapsed_seconds": round(elapsed, 3), "status": "ok", **result}
    except TimeoutError:
        print(f"  TIMEOUT (>{TIMEOUT}s)")
        return {"framework": name, "elapsed_seconds": TIMEOUT, "status": "timeout"}
    except Exception as e:
        signal.alarm(0)
        print(f"  ERROR: {e}")
        return {"framework": name, "elapsed_seconds": 0, "status": f"error: {e}"}


# ─── Barca ────────────────────────────────────────────────────────────────────


def run_barca():
    # Generate pipeline file
    lines = ["from barca import asset, partitions", ""]
    lines.append(f'TICKERS = [f"T{{i:04d}}" for i in range({len(TICKERS)})]')
    lines.append("")
    lines.append('@asset(partitions={"ticker": partitions(TICKERS)})')
    lines.append("def step_0(ticker):")
    lines.append('    return {"ticker": ticker, "v": 0}')
    lines.append("")
    for i in range(1, 10):
        lines.append(
            f'@asset(inputs={{"data": step_{i - 1}}}, partitions={{"ticker": partitions(TICKERS)}})'
        )
        lines.append(f"def step_{i}(data, ticker):")
        lines.append(f'    return {{"ticker": ticker, "v": data["v"] + {i}}}')
        lines.append("")

    f = "/tmp/barca_bench.py"
    with open(f, "w") as fh:
        fh.write("\n".join(lines))

    shutil.rmtree("/tmp/.barca", ignore_errors=True)
    os.environ.pop("BARCA_HOME", None)

    r = subprocess.run(
        ["barca", "get", "step_9", f, "--refresh-all", "-o", "json"],
        capture_output=True,
        text=True,
        cwd="/tmp",
    )
    if r.returncode != 0:
        raise RuntimeError(r.stderr[:200])
    last = [line for line in r.stdout.strip().splitlines() if line.startswith("{")][-1]
    d = json.loads(last)
    return {"steps": d["steps_executed"], "pattern": "partitions()"}


# ─── Dagster ──────────────────────────────────────────────────────────────────


def run_dagster():
    from dagster import asset, StaticPartitionsDefinition, materialize

    ticker_partitions = StaticPartitionsDefinition(TICKERS)

    @asset(partitions_def=ticker_partitions)
    def step_0(context):
        return {"ticker": context.partition_key, "v": 0}

    @asset(partitions_def=ticker_partitions)
    def step_1(context, step_0):
        return {"ticker": context.partition_key, "v": step_0["v"] + 1}

    @asset(partitions_def=ticker_partitions)
    def step_2(context, step_1):
        return {"ticker": context.partition_key, "v": step_1["v"] + 2}

    @asset(partitions_def=ticker_partitions)
    def step_3(context, step_2):
        return {"ticker": context.partition_key, "v": step_2["v"] + 3}

    @asset(partitions_def=ticker_partitions)
    def step_4(context, step_3):
        return {"ticker": context.partition_key, "v": step_3["v"] + 4}

    @asset(partitions_def=ticker_partitions)
    def step_5(context, step_4):
        return {"ticker": context.partition_key, "v": step_4["v"] + 5}

    @asset(partitions_def=ticker_partitions)
    def step_6(context, step_5):
        return {"ticker": context.partition_key, "v": step_5["v"] + 6}

    @asset(partitions_def=ticker_partitions)
    def step_7(context, step_6):
        return {"ticker": context.partition_key, "v": step_6["v"] + 7}

    @asset(partitions_def=ticker_partitions)
    def step_8(context, step_7):
        return {"ticker": context.partition_key, "v": step_7["v"] + 8}

    @asset(partitions_def=ticker_partitions)
    def step_9(context, step_8):
        return {"ticker": context.partition_key, "v": step_8["v"] + 9}

    all_assets = [step_0, step_1, step_2, step_3, step_4, step_5, step_6, step_7, step_8, step_9]
    for ticker in TICKERS:
        materialize(all_assets, partition_key=ticker)

    return {"steps": len(TICKERS) * 10, "pattern": "StaticPartitionsDefinition"}


# ─── Prefect ──────────────────────────────────────────────────────────────────


def run_prefect():
    from prefect import flow, task

    @task
    def pf_step_0(ticker):
        return {"ticker": ticker, "v": 0}

    @task
    def pf_step_1(data, ticker):
        return {"ticker": ticker, "v": data["v"] + 1}

    @task
    def pf_step_2(data, ticker):
        return {"ticker": ticker, "v": data["v"] + 2}

    @task
    def pf_step_3(data, ticker):
        return {"ticker": ticker, "v": data["v"] + 3}

    @task
    def pf_step_4(data, ticker):
        return {"ticker": ticker, "v": data["v"] + 4}

    @task
    def pf_step_5(data, ticker):
        return {"ticker": ticker, "v": data["v"] + 5}

    @task
    def pf_step_6(data, ticker):
        return {"ticker": ticker, "v": data["v"] + 6}

    @task
    def pf_step_7(data, ticker):
        return {"ticker": ticker, "v": data["v"] + 7}

    @task
    def pf_step_8(data, ticker):
        return {"ticker": ticker, "v": data["v"] + 8}

    @task
    def pf_step_9(data, ticker):
        return {"ticker": ticker, "v": data["v"] + 9}

    @flow
    def pipeline():
        r0 = pf_step_0.map(TICKERS)
        r1 = pf_step_1.map(r0, TICKERS)
        r2 = pf_step_2.map(r1, TICKERS)
        r3 = pf_step_3.map(r2, TICKERS)
        r4 = pf_step_4.map(r3, TICKERS)
        r5 = pf_step_5.map(r4, TICKERS)
        r6 = pf_step_6.map(r5, TICKERS)
        r7 = pf_step_7.map(r6, TICKERS)
        r8 = pf_step_8.map(r7, TICKERS)
        return pf_step_9.map(r8, TICKERS)

    pipeline()
    return {"steps": 10000, "pattern": "task.map()"}


# ─── Main ─────────────────────────────────────────────────────────────────────

if __name__ == "__main__":
    results = []
    results.append(timed_run("Barca (partitions())", run_barca))
    results.append(timed_run("Dagster (StaticPartitionsDefinition)", run_dagster))
    results.append(timed_run("Prefect (task.map())", run_prefect))

    print(f"\n{'=' * 60}")
    print("  RESULTS: 10 steps × 1000 partitions = 10,000 steps")
    print(f"{'=' * 60}")
    print(f"  {'Framework':<40} {'Time':>10} {'Status':<10}")
    print(f"  {'-' * 60}")
    for r in results:
        t = f"{r['elapsed_seconds']:.1f}s" if r["status"] == "ok" else r["status"]
        print(f"  {r['framework']:<40} {t:>10} {r.get('pattern', '')}")

    with open("/bench/results.json", "w") as f:
        json.dump(results, f, indent=2)
    print("\nResults saved to /bench/results.json")
