"""Run the Airflow DAG with `airflow dags test`: fan_out_500_50ms.

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
    airflow_home = tempfile.mkdtemp(prefix="airflow_bench_")
    dags_dir = os.path.join(airflow_home, "dags")
    os.makedirs(dags_dir)
    shutil.copy(os.path.join(SCRIPT_DIR, "dag.py"), dags_dir)

    env = {
        **os.environ,
        "AIRFLOW_HOME": airflow_home,
        "AIRFLOW__CORE__DAGS_FOLDER": dags_dir,
        "AIRFLOW__CORE__LOAD_EXAMPLES": "False",
        "AIRFLOW__CORE__LOAD_DEFAULT_CONNECTIONS": "False",
        "AIRFLOW__CORE__EXECUTOR": "LocalExecutor",
        "AIRFLOW__CORE__PARALLELISM": "32",
        "AIRFLOW__CORE__MAX_ACTIVE_TASKS_PER_DAG": "500",
        "AIRFLOW__DATABASE__SQL_ALCHEMY_CONN": f"sqlite:///{airflow_home}/airflow.db",
        "AIRFLOW__LOGGING__LOGGING_LEVEL": "ERROR",
    }

    # Initialize DB
    subprocess.run([AIRFLOW_BIN, "db", "migrate"], env=env, capture_output=True)

    # `dags test` without --use-executor runs the tasks in process, sequentially.
    t0 = time.perf_counter()
    result = subprocess.run(
        [
            AIRFLOW_BIN,
            "dags",
            "test",
            "fan_out_500_50ms",
            "2024-01-01",
        ],
        env=env,
        capture_output=True,
        text=True,
    )
    elapsed = time.perf_counter() - t0

    shutil.rmtree(airflow_home, ignore_errors=True)

    print(
        json.dumps(
            {
                "elapsed_seconds": round(elapsed, 6),
                "steps_executed": 500,
                "success": result.returncode == 0,
                "executor": "dags test (in process)",
            },
            indent=2,
        )
    )


if __name__ == "__main__":
    run()
