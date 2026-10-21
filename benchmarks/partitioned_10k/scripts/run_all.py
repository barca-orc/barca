"""10 steps × 1000 partitions = 10,000 steps, each framework in its idiomatic pattern.

    python run_all.py                      # barca, dagster, prefect in this interpreter (Docker)
    python run_all.py dagster              # one framework; run it with that framework's venv
    python run_all.py airflow              # needs AIRFLOW_BIN and AIRFLOW_SQL_ALCHEMY_CONN (PostgreSQL)

Each framework runs in a child process that is killed after BENCH_TIMEOUT seconds (default 600,
10 minutes); a killed run is reported as "timeout". barca is the `barca` on PATH, or $BARCA.
Results go to $BENCH_RESULTS (default results.json next to this script).
"""

import json
import multiprocessing
import os
import queue as queue_mod
import shutil
import signal
import subprocess
import sys
import tempfile
import time

TICKERS = [f"T{i:04d}" for i in range(1000)]
TIMEOUT = int(os.environ.get("BENCH_TIMEOUT", "600"))  # 10 minutes


def _child(fn, queue):
    os.setsid()  # own process group, so a timeout also kills servers the framework started
    try:
        queue.put(("ok", fn()))
    except Exception as e:  # noqa: BLE001 -- reported, not raised: the parent prints the table
        queue.put(("error", str(e)[:300]))


def timed_run(name, fn):
    """Run a benchmark function in a child process, killed after TIMEOUT seconds."""
    print(f"\n{'=' * 60}")
    print(f"  {name}")
    print(f"{'=' * 60}", flush=True)
    queue = multiprocessing.Queue()
    proc = multiprocessing.Process(target=_child, args=(fn, queue))
    t0 = time.perf_counter()
    proc.start()
    proc.join(TIMEOUT)
    elapsed = time.perf_counter() - t0
    if proc.is_alive():
        os.killpg(proc.pid, signal.SIGKILL)
        proc.join()
        print(f"  TIMEOUT (killed after {TIMEOUT}s)")
        return {"framework": name, "elapsed_seconds": TIMEOUT, "status": "timeout"}
    try:
        status, payload = queue.get(timeout=5)
    except queue_mod.Empty:
        status, payload = "error", f"exit {proc.exitcode}"
    if status != "ok":
        print(f"  ERROR: {payload}")
        return {
            "framework": name,
            "elapsed_seconds": round(elapsed, 3),
            "status": f"error: {payload}",
        }
    print(f"  Result: {elapsed:.3f}s")
    return {"framework": name, "elapsed_seconds": round(elapsed, 3), "status": "ok", **payload}


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

    work = tempfile.mkdtemp(prefix="barca_10k_")
    f = os.path.join(work, "barca_bench.py")
    with open(f, "w") as fh:
        fh.write("\n".join(lines))
    os.environ.pop("BARCA_HOME", None)

    try:
        r = subprocess.run(
            [os.environ.get("BARCA", "barca"), "get", "step_9", f, "--refresh-all", "--json"],
            capture_output=True,
            text=True,
            cwd=work,
            check=False,
        )
    finally:
        shutil.rmtree(work, ignore_errors=True)
    if r.returncode != 0:
        raise RuntimeError(r.stderr[-300:])
    d = json.loads(r.stdout)
    return {"steps": d["steps_executed"], "pattern": "partitions()"}


# ─── Dagster ──────────────────────────────────────────────────────────────────


def run_dagster():
    os.environ.setdefault("DAGSTER_HOME", tempfile.mkdtemp(prefix="dagster_10k_"))
    from dagster import StaticPartitionsDefinition, asset, materialize

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
    os.environ.setdefault("PREFECT_HOME", tempfile.mkdtemp(prefix="prefect_10k_"))
    os.environ.setdefault("PREFECT_LOGGING_LEVEL", "CRITICAL")
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


# ─── Airflow ──────────────────────────────────────────────────────────────────


def run_airflow():
    """`airflow dags test` on ../airflow/dags/partitioned_chain.py (expand()), PostgreSQL metadata.

    `dags test` without --use-executor runs the mapped task instances in process, one after
    another. `airflow db migrate` runs first and is included in the time.
    """
    airflow = os.environ["AIRFLOW_BIN"]
    home = tempfile.mkdtemp(prefix="airflow_10k_")
    dags = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "airflow", "dags")
    env = {
        **os.environ,
        "AIRFLOW_HOME": home,
        "AIRFLOW__CORE__DAGS_FOLDER": os.path.abspath(dags),
        "AIRFLOW__CORE__LOAD_EXAMPLES": "False",
        "AIRFLOW__CORE__MAX_ACTIVE_TASKS_PER_DAG": "10000",
        "AIRFLOW__DATABASE__SQL_ALCHEMY_CONN": os.environ["AIRFLOW_SQL_ALCHEMY_CONN"],
        "AIRFLOW__LOGGING__LOGGING_LEVEL": "ERROR",
    }
    try:
        for cmd in (["db", "migrate"], ["dags", "test", "partitioned_chain", "2024-01-01"]):
            r = subprocess.run(
                [airflow, *cmd], env=env, capture_output=True, text=True, check=False
            )
            if r.returncode != 0:
                raise RuntimeError(f"airflow {' '.join(cmd)}: {r.stderr[-300:]}")
    finally:
        shutil.rmtree(home, ignore_errors=True)
    return {"steps": 10000, "pattern": "expand() + PostgreSQL"}


# ─── Main ─────────────────────────────────────────────────────────────────────

FRAMEWORKS = {
    "barca": ("Barca (partitions())", run_barca),
    "dagster": ("Dagster (StaticPartitionsDefinition)", run_dagster),
    "prefect": ("Prefect (task.map())", run_prefect),
    "airflow": ("Airflow (expand())", run_airflow),
}

if __name__ == "__main__":
    multiprocessing.set_start_method("fork")
    chosen = sys.argv[1:] or ["barca", "dagster", "prefect"]
    results = [timed_run(*FRAMEWORKS[name]) for name in chosen]

    print(f"\n{'=' * 60}")
    print("  RESULTS: 10 steps × 1000 partitions = 10,000 steps")
    print(f"{'=' * 60}")
    print(f"  {'Framework':<40} {'Time':>10} {'Status':<10}")
    print(f"  {'-' * 60}")
    for r in results:
        t = f"{r['elapsed_seconds']:.1f}s" if r["status"] == "ok" else r["status"]
        print(f"  {r['framework']:<40} {t:>10} {r.get('pattern', '')}")

    out = os.environ.get(
        "BENCH_RESULTS", os.path.join(os.path.dirname(os.path.abspath(__file__)), "results.json")
    )
    with open(out, "w") as f:
        json.dump(results, f, indent=2)
    print(f"\nResults saved to {out}")
