"""Run the Airflow DAG with `airflow dags test`: trivial.

`dags test` runs every task in this process, one after another: without `--use-executor` it
uses no executor, so the AIRFLOW__CORE__EXECUTOR setting below has no effect on it. Only the
`dags test` call is timed; `airflow db migrate` runs first, untimed. Set AIRFLOW_BIN to an
airflow executable outside the repository (default: ./.venv/bin/airflow).
"""

import json
import os
import shutil
import subprocess
import tempfile
import time

SCRIPT_DIR = os.path.dirname(os.path.abspath(__file__))
AIRFLOW_BIN = os.environ.get("AIRFLOW_BIN") or os.path.join(SCRIPT_DIR, ".venv", "bin", "airflow")


def run():
    ah = tempfile.mkdtemp(prefix="airflow_bench_")
    dd = os.path.join(ah, "dags")
    os.makedirs(dd)
    shutil.copy(os.path.join(SCRIPT_DIR, "dag.py"), dd)
    env = {
        **os.environ,
        "AIRFLOW_HOME": ah,
        "AIRFLOW__CORE__DAGS_FOLDER": dd,
        "AIRFLOW__CORE__LOAD_EXAMPLES": "False",
        "AIRFLOW__CORE__LOAD_DEFAULT_CONNECTIONS": "False",
        "AIRFLOW__CORE__EXECUTOR": "LocalExecutor",
        "AIRFLOW__DATABASE__SQL_ALCHEMY_CONN": f"sqlite:///{ah}/airflow.db",
        "AIRFLOW__LOGGING__LOGGING_LEVEL": "ERROR",
    }
    subprocess.run([AIRFLOW_BIN, "db", "migrate"], env=env, capture_output=True)
    t0 = time.perf_counter()
    result = subprocess.run(
        [
            AIRFLOW_BIN,
            "dags",
            "test",
            "trivial",
            "2024-01-01",
        ],
        env=env,
        capture_output=True,
        text=True,
    )
    elapsed = time.perf_counter() - t0
    shutil.rmtree(ah, ignore_errors=True)
    print(
        json.dumps(
            {
                "elapsed_seconds": round(elapsed, 6),
                "steps_executed": 1,
                "success": result.returncode == 0,
                "executor": "dags test (in process)",
            },
            indent=2,
        )
    )


if __name__ == "__main__":
    run()
